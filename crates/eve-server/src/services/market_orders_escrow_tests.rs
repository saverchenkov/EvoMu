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

    // Self-trade BuyMultipleItems emits buyer debit OnAccountChange before seller credit (#789)
    // and PlaceMultiSellOrder accepts integer 0 for useCorp (#795)
    ctx.db
        .acquire_online_lock(1, 9301, Some(buyer_id), true, ctx.now_unix())
        .unwrap();
    let self_stack_id = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let sid = s.next_entity_id;
        s.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id: 34,
                owner_id: buyer_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 20,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        sid
    });
    let sell_entry = PyRep::keyval([
        ("stationID", PyRep::Integer(JITA_STATION_ID)),
        ("typeID", PyRep::Integer(34)),
        ("itemID", PyRep::Integer(self_stack_id)),
        ("price", PyRep::Real(8.0)),
        ("quantity", PyRep::Integer(20)),
    ]);
    handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![sell_entry]),
            PyRep::Integer(0), // integer boolish useCorp
            PyRep::Integer(90),
            PyRep::None,
        ],
        &kw,
    )
    .unwrap()
    .expect("PlaceMultiSellOrder with Integer(0) useCorp");

    let _ = ctx.drain_notifications_for_char(buyer_id);
    let self_buy_reqs = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(34),
        PyRep::Integer(10),
        PyRep::Real(8.0),
    ])]);
    handle_orders_escrow(
        &ctx,
        &mut sess,
        "marketProxy",
        "BuyMultipleItems",
        &[self_buy_reqs],
        &kw,
    )
    .unwrap()
    .unwrap();
    let final_bal = ctx.db.get_character(buyer_id).unwrap().balance.to_wire_f64();
    let acct_bals: Vec<f64> = ctx
        .drain_notifications_for_char(buyer_id)
        .into_iter()
        .filter(|n| n.event == "OnAccountChange")
        .filter_map(|n| n.args.get(2).and_then(PyRep::as_f64))
        .collect();
    assert!(acct_bals.len() >= 2);
    assert!(acct_bals[0] < acct_bals[1]);
    assert!((acct_bals.last().copied().unwrap() - final_bal).abs() < 1e-2);
}

fn call_escrow_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle_orders_escrow(ctx, sess, "marketProxy", method, args, kw)
        .expect("handled by marketProxy escrow")
}

fn seed_upwell_market_structure(
    ctx: &ServerContext,
    structure_id: i64,
    owner_corp_id: i64,
    solar_system_id: u32,
    online: bool,
) {
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                solar_system_id,
                owner_corp_id,
                type_id: 35833, // Fortizar
                name: format!("Market-{structure_id}"),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: if online { 500 } else { 0 },
                ..Default::default()
            },
        );
        let mod_id = structure_id + 1;
        s.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 35892, // Standup Market Hub I
                owner_id: owner_corp_id,
                location_id: structure_id,
                flag: InventoryFlag::from_u16(164),
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        s.module_online_states.insert(mod_id, online);
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(s);
        ext.access_profiles.insert(
            structure_id,
            crate::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                corp_only: false,
                ..Default::default()
            },
        );
        crate::services::station_structure::structure_upwell_ops::save_ext_state(s, &ext);
    });
}

#[test]
fn market_orders_corp_auth_role_matrix_and_sellability_guards() {
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, AssetSafetyWrapRecord,
    };
    use eve_db::corp_roles::{
        ROLE_ACCOUNTANT, ROLE_ACCOUNT_CAN_TAKE, ROLE_DELIVERIES_HANGAR_TAKE, ROLE_DIRECTOR,
        ROLE_HANGAR_CAN_TAKE, ROLE_TRADER,
    };
    use eve_db::{CorpMemberRoleAssignment, CorporationRecord, IskCents};

    let ctx = test_ctx();
    let now = ctx.now_unix();
    let corp_id = 98_910_001_i64;
    let ceo_id = 910_001_i64;
    let dir_id = 910_002_i64;
    let acct_id = 910_003_i64;
    let base_trader_id = 910_004_i64;
    let other_station = 60_003_759_i64;
    let base_station = 60_003_758_i64;
    let hq_station = JITA_STATION_ID;

    let make_role = |character_id, roles, roles_at_hq, roles_at_base, roles_at_other| {
        CorpMemberRoleAssignment {
            character_id,
            roles,
            grantable_roles: 0,
            roles_at_hq,
            grantable_roles_at_hq: 0,
            roles_at_base,
            grantable_roles_at_base: 0,
            roles_at_other,
            grantable_roles_at_other: 0,
        }
    };

    ctx.db.with_mut_snapshot(|s| {
        let base_char = s.characters.values().next().unwrap().clone();
        for cid in [ceo_id, dir_id, acct_id, base_trader_id] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.name = format!("AuthChar-{cid}");
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(JITA_STATION_ID);
            ch.balance = IskCents(1_000_000_000);
            s.characters.insert(cid, ch);
        }
        let mut auth_corp = CorporationRecord::new(corp_id, "Auth Corp", "AUTH", ceo_id)
            .with_master_wallet(IskCents(5_000_000_000));
        auth_corp.hq_station_id = Some(hq_station);
        s.corporations.insert(corp_id, auth_corp);
        s.corp_governance
            .entry(corp_id)
            .or_default()
            .member_bases
            .insert(base_trader_id, base_station);

        s.corp_role_assignments
            .insert((corp_id, dir_id), make_role(dir_id, ROLE_DIRECTOR, 0, 0, 0));
        // Accountant has ROLE_ACCOUNTANT + ONLY division 3 take (accountKey 1002 -> index 2)
        // plus DELIVERIES take and SAG4 take (index 3) and SAG7 take (index 6)
        s.corp_role_assignments.insert(
            (corp_id, acct_id),
            make_role(
                acct_id,
                ROLE_ACCOUNTANT
                    | ROLE_DELIVERIES_HANGAR_TAKE
                    | ROLE_HANGAR_CAN_TAKE[3]
                    | ROLE_HANGAR_CAN_TAKE[6]
                    | ROLE_ACCOUNT_CAN_TAKE[2],
                0,
                0,
                0,
            ),
        );
        s.corp_role_assignments.insert(
            (corp_id, base_trader_id),
            make_role(
                base_trader_id,
                0,
                ROLE_ACCOUNT_CAN_TAKE[0],
                ROLE_TRADER,
                ROLE_HANGAR_CAN_TAKE[1],
            ),
        );
    });

    ctx.db.with_snapshot(|s| {
        // effective_corp_roles_at_station_in_snap: CEO, missing corp, hq, base, other
        assert_eq!(
            market_orders_corp_auth::effective_corp_roles_at_station_in_snap(
                s, corp_id, ceo_id, hq_station, now
            ),
            (true, !0u64)
        );
        assert_eq!(
            market_orders_corp_auth::effective_corp_roles_at_station_in_snap(
                s, 98_999_888, ceo_id, hq_station, now
            ),
            (false, 0)
        );
        let (_, r_hq) = market_orders_corp_auth::effective_corp_roles_at_station_in_snap(
            s,
            corp_id,
            base_trader_id,
            hq_station,
            now,
        );
        assert_eq!(r_hq, ROLE_ACCOUNT_CAN_TAKE[0]);
        let (_, r_base) = market_orders_corp_auth::effective_corp_roles_at_station_in_snap(
            s,
            corp_id,
            base_trader_id,
            base_station,
            now,
        );
        assert_eq!(r_base, ROLE_TRADER);
        let (_, r_other) = market_orders_corp_auth::effective_corp_roles_at_station_in_snap(
            s,
            corp_id,
            base_trader_id,
            other_station,
            now,
        );
        assert_eq!(r_other, ROLE_HANGAR_CAN_TAKE[1]);

        // require_corp_market_order_role missing corp + unauthorized member
        assert_eq!(
            market_orders_corp_auth::require_corp_market_order_role(
                s, 98_999_888, ceo_id, 1000, now
            )
            .unwrap_err()
            .code,
            "CorporationNotFound"
        );
        assert_eq!(
            market_orders_corp_auth::require_corp_market_order_role(
                s, corp_id, base_trader_id, 1000, now
            )
            .unwrap_err()
            .code,
            "CorpRoleRequired"
        );

        // require_corp_accountant_spend_role branches: missing corp, CEO, Director, Accountant matching vs mismatching div, denied member
        assert_eq!(
            market_orders_corp_auth::require_corp_accountant_spend_role(
                s, 98_999_888, ceo_id, 1000, now, "SpendDenied"
            )
            .unwrap_err()
            .code,
            "CorporationNotFound"
        );
        assert!(market_orders_corp_auth::require_corp_accountant_spend_role(
            s, corp_id, ceo_id, 1006, now, "SpendDenied"
        )
        .is_ok());
        assert!(market_orders_corp_auth::require_corp_accountant_spend_role(
            s, corp_id, dir_id, 1001, now, "SpendDenied"
        )
        .is_ok());
        assert!(market_orders_corp_auth::require_corp_accountant_spend_role(
            s, corp_id, acct_id, 1002, now, "SpendDenied"
        )
        .is_ok());
        assert_eq!(
            market_orders_corp_auth::require_corp_accountant_spend_role(
                s, corp_id, acct_id, 1000, now, "SpendDenied"
            )
            .unwrap_err()
            .code,
            "SpendDenied"
        );
        assert_eq!(
            market_orders_corp_auth::require_corp_accountant_spend_role(
                s,
                corp_id,
                base_trader_id,
                1002,
                now,
                "SpendDenied"
            )
            .unwrap_err()
            .code,
            "SpendDenied"
        );
    });

    // is_item_sellable_on_market: controller saved ship, type 60/3468 wrap, asset safety wrap, CorpDeliveries, CorpSAG1..7, disallowed flag
    ctx.db.with_mut_snapshot(|s| {
        let mut ext = load_ext_state(s);
        ext.active_pilot_by_structure.insert(10_000_910_001, ceo_id);
        ext.previous_ship_by_structure.insert(10_000_910_001, 910_500);
        ext.asset_safety_wraps.insert(
            910_600,
            AssetSafetyWrapRecord {
                wrap_id: 910_600,
                owner_id: ceo_id,
                is_corp: false,
                source_structure_id: 10_000_910_001,
                source_structure_name: "Saved".into(),
                solar_system_id: 30_000_142,
                created_unix: now,
                deliverable_after_unix: now,
                delivered_to_id: Some(JITA_STATION_ID),
                item_ids: vec![910_602],
                estimated_value_isk: 100.0,
                nearest_npc_station_id: Some(JITA_STATION_ID),
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        save_ext_state(s, &ext);
    });

    ctx.db.with_snapshot(|s| {
        let ext = load_ext_state(s);
        let base_item = |id: i64, type_id: u32, owner_id: i64, loc: i64, flag: InventoryFlag| {
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id,
                location_id: loc,
                flag,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            }
        };
        // Saved controller ship rejected
        assert!(!market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_500, 587, ceo_id, hq_station, InventoryFlag::Hangar),
            ceo_id,
            corp_id,
            false,
            hq_station,
            None,
            now,
        ));
        // Asset safety / plastic wrap type_id 60 and 3468 rejected
        assert!(!market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_501, 60, ceo_id, hq_station, InventoryFlag::Hangar),
            ceo_id,
            corp_id,
            false,
            hq_station,
            None,
            now,
        ));
        assert!(!market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_502, 3_468, ceo_id, hq_station, InventoryFlag::Hangar),
            ceo_id,
            corp_id,
            false,
            hq_station,
            None,
            now,
        ));
        // Unretrieved safety wrap by wrap_id, location_id, and item_ids all rejected
        assert!(!market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_600, 34, ceo_id, hq_station, InventoryFlag::Hangar),
            ceo_id,
            corp_id,
            false,
            hq_station,
            None,
            now,
        ));
        assert!(!market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_601, 34, ceo_id, 910_600, InventoryFlag::Hangar),
            ceo_id,
            corp_id,
            false,
            hq_station,
            None,
            now,
        ));
        assert!(!market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_602, 34, ceo_id, hq_station, InventoryFlag::Hangar),
            ceo_id,
            corp_id,
            false,
            hq_station,
            None,
            now,
        ));

        // CorpDeliveries + CorpSAG1..7 + Cargo flags
        assert!(market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_700, 34, corp_id, hq_station, InventoryFlag::CorpDeliveries),
            acct_id,
            corp_id,
            true,
            hq_station,
            None,
            now,
        ));
        assert!(!market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_701, 34, corp_id, hq_station, InventoryFlag::CorpDeliveries),
            base_trader_id,
            corp_id,
            true,
            hq_station,
            None,
            now,
        ));
        assert!(market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_702, 34, corp_id, hq_station, InventoryFlag::CorpSAG4),
            acct_id,
            corp_id,
            true,
            hq_station,
            None,
            now,
        ));
        assert!(market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_702_1, 34, corp_id, hq_station, InventoryFlag::CorpSAG7),
            acct_id,
            corp_id,
            true,
            hq_station,
            None,
            now,
        ));
        assert!(!market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_703, 34, corp_id, hq_station, InventoryFlag::CorpSAG4),
            base_trader_id,
            corp_id,
            true,
            hq_station,
            None,
            now,
        ));
        assert!(market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_703_1, 34, corp_id, other_station, InventoryFlag::CorpSAG2),
            base_trader_id,
            corp_id,
            true,
            other_station,
            None,
            now,
        ));
        assert!(!market_orders_corp_auth::is_item_sellable_on_market(
            s,
            &ext,
            &base_item(910_704, 34, corp_id, hq_station, InventoryFlag::Cargo),
            ceo_id,
            corp_id,
            true,
            hq_station,
            None,
            now,
        ));
    });
}

#[test]
fn escrow_multi_sell_upwell_errors_overflow_and_cancel_edge_paths() {
    use eve_db::{CorporationRecord, MarketOrderMeta};

    let ctx = test_ctx();
    let now = ctx.now_unix();
    let char_id = 920_001_i64;
    let corp_id = 98_920_001_i64;
    let owner_corp_id = 98_920_002_i64;
    let online_fort_id = 10_000_920_001_i64;
    let offline_fort_id = 10_000_920_002_i64;
    let wrong_region_fort_id = 10_000_920_003_i64;

    ctx.db.with_mut_snapshot(|s| {
        let base_char = s.characters.values().next().unwrap().clone();
        let mut ch = base_char;
        ch.character_id = char_id;
        ch.name = "EscrowTester".into();
        ch.corporation_id = corp_id;
        ch.solar_system_id = 30_000_142;
        ch.station_id = Some(JITA_STATION_ID);
        ch.balance = IskCents(1_000_000_000);
        s.characters.insert(char_id, ch);

        let mut corp = CorporationRecord::new(corp_id, "Escrow Corp", "ESCR", char_id)
            .with_master_wallet(IskCents(500_000_000));
        corp.hq_station_id = Some(JITA_STATION_ID);
        corp.set_division_balance(3, IskCents(1_000_000));
        s.corporations.insert(corp_id, corp);

        s.corporations.insert(
            owner_corp_id,
            CorporationRecord::new(owner_corp_id, "Owner Corp", "OWNR", char_id),
        );
    });
    seed_upwell_market_structure(&ctx, online_fort_id, owner_corp_id, 30_000_142, true);
    seed_upwell_market_structure(&ctx, offline_fort_id, owner_corp_id, 30_000_142, false);
    seed_upwell_market_structure(&ctx, wrong_region_fort_id, owner_corp_id, 30_002_187, true); // Amarr != Forge

    let mut sess = jita_session(char_id, corp_id);
    let mut kw = HashMap::new();

    // Unhandled method returns None (L1462)
    assert!(handle_orders_escrow(&ctx, &mut sess, "marketProxy", "NoSuchMethod", &[], &kw).is_none()); // rpc-lint: allow test fixture

    // Invalid kwargs types for useCorp / duration (L69, L73)
    kw.insert("useCorp".into(), PyRep::String("bad".into()));
    assert!(call_escrow_kw(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![])], &kw).is_err());
    kw.clear();
    kw.insert("duration".into(), PyRep::String("bad".into()));
    assert!(call_escrow_kw(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![])], &kw).is_err());
    kw.clear();

    // Unknown stationID, offline structure, wrong-region structure in PlaceMultiSellOrder
    let entry_trit = |tid: u32, iid: i64, price: f64, qty: i64| {
        PyRep::keyval([
            ("itemID", PyRep::Integer(iid)),
            ("typeID", PyRep::Integer(i64::from(tid))),
            ("price", PyRep::Real(price)),
            ("quantity", PyRep::Integer(qty)),
        ])
    };
    sess.station_id = Some(69_999_999);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![entry_trit(34, 0, 10.0, 1)])], &kw)
            .unwrap_err()
            .code,
        "InvalidMarketStation"
    );
    sess.station_id = Some(offline_fort_id);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![entry_trit(34, 0, 10.0, 1)])], &kw)
            .unwrap_err()
            .code,
        "MarketHubOffline"
    );
    sess.station_id = Some(wrong_region_fort_id);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "PlaceMultiSellOrder", &[PyRep::List(vec![entry_trit(34, 0, 10.0, 1)])], &kw)
            .unwrap_err()
            .code,
        "InvalidMarketStation"
    );

    // Online Upwell Fortizar: places resting sell order, uses structure broker fee, credits owner corp div1!
    sess.station_id = Some(online_fort_id);
    let fort_item_id = 920_100_i64;
    let fort_ship_id = 920_101_i64;
    let locked_id = 920_102_i64;
    let box_id = 920_103_i64;
    let box_child_id = 920_104_i64;
    let fitted_dmg_ship_id = 920_105_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.retain(|_, i| i.location_id != JITA_STATION_ID);
        s.items.insert(
            fort_item_id,
            InventoryItemRecord {
                item_id: fort_item_id,
                type_id: 34,
                owner_id: char_id,
                location_id: online_fort_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            fort_ship_id,
            InventoryItemRecord {
                item_id: fort_ship_id,
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
        s.items.insert(
            locked_id,
            InventoryItemRecord {
                item_id: locked_id,
                type_id: 35,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        s.contracts.insert(
            920_999,
            eve_db::ContractRecord {
                contract_id: 920_999,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: JITA_STATION_ID,
                end_station_id: JITA_STATION_ID,
                item_ids: vec![locked_id],
                date_issued_unix: now,
                expiration_unix: now + 3600,
                ..Default::default()
            },
        );
        s.items.insert(
            box_id,
            InventoryItemRecord {
                item_id: box_id,
                type_id: 3293,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            box_child_id,
            InventoryItemRecord {
                item_id: box_child_id,
                type_id: 37,
                owner_id: char_id,
                location_id: box_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            fitted_dmg_ship_id,
            InventoryItemRecord {
                item_id: fitted_dmg_ship_id,
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
    });

    // Mark fitted_dmg_ship_id with rack_heat > 0 to exercise damaged_fitted_ships filter (L163-165, L851-852)
    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, fitted_dmg_ship_id).unwrap();
    fit.rack_heat = [4.0, 0.0, 0.0];
    ctx.store_ship_fitting(fitted_dmg_ship_id, fit);

    // Rate exceeded and ISK amount exceeded via expectedBrokersFee positional args[3] (L781-795)
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![entry_trit(34, fort_item_id, 1_000.0, 10)]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0001),
            ],
            &kw,
        )
        .unwrap_err()
        .code,
        "BrokerFeeExceeded"
    );
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![entry_trit(34, fort_item_id, 1_000.0, 10)]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(2.0), // > 1.0 ISK amount threshold exceeded
            ],
            &kw,
        )
        .unwrap_err()
        .code,
        "BrokerFeeExceeded"
    );

    // Succeed at online Upwell Fortizar -> credits owner_corp_id div 1!
    let owner_bal_before =
        ctx.db.with_snapshot(|s| s.corporations[&owner_corp_id].master_wallet_balance());
    let fort_res = call_escrow_kw(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![entry_trit(34, fort_item_id, 1_000.0, 10)])],
        &kw,
    )
    .unwrap();
    let fort_oid = fort_res.dict_get("orderID").and_then(PyRep::as_i64).unwrap();
    let fort_broker = fort_res.dict_get("brokerFeeISK").and_then(PyRep::as_f64).unwrap();
    assert!(fort_broker > 0.0);
    ctx.db.with_snapshot(|s| {
        assert!(s.corporations[&owner_corp_id].master_wallet_balance() > owner_bal_before);
    });

    // Switch back to Jita NPC station to cover:
    // - CantRepackageDamagedItem with rack_heat
    // - CannotSellAssembledItem auto-discovery
    // - InsufficientInventoryForSellOrder auto-discovery & wrong explicit itemID
    // - ItemLocked from s.locked_items
    // - MarketOrderOverflow (huge price)
    // - Broker fee InsufficientFunds (personal & corp)
    // - MaxOpenOrdersReached
    sess.station_id = Some(JITA_STATION_ID);
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![entry_trit(587, fitted_dmg_ship_id, 10_000.0, 1)])],
            &kw,
        )
        .unwrap_err()
        .code,
        "CantRepackageDamagedItem"
    );
    // Remove fitted_dmg_ship_id; only assembled fort_ship_id (587) remains -> auto-discovery CannotSellAssembledItem
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&fitted_dmg_ship_id);
    });
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![entry_trit(587, 0, 10_000.0, 1)])],
            &kw,
        )
        .unwrap_err()
        .code,
        "CannotSellAssembledItem"
    );
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![entry_trit(34, 0, 10.0, 5)])],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientInventoryForSellOrder"
    );
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![entry_trit(34, box_child_id, 10.0, 1)])],
            &kw,
        )
        .unwrap_err()
        .code,
        "ItemInsideContainer"
    );
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![entry_trit(35, locked_id, 10.0, 1)])],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientInventoryForSellOrder"
    );

    // Explicit stack of trit in Jita + MarketOrderOverflow (huge price) + broker InsufficientFunds
    let jita_trit_id = ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            920_200,
            InventoryItemRecord {
                item_id: 920_200,
                type_id: 34,
                owner_id: char_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        920_200
    });
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![entry_trit(34, jita_trit_id, 1e13, 10_000)])],
            &kw,
        )
        .unwrap_err()
        .code,
        "MarketOrderOverflow"
    );
    // Personal InsufficientFunds on broker fee (0 ISK balance)
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().balance = IskCents(1);
    });
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![entry_trit(34, jita_trit_id, 1_000_000.0, 1)])],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    // Corp InsufficientFunds on broker fee (corp div 1 = 1 ISK cent)
    let corp_trit_id = ctx.db.with_mut_snapshot(|s| {
        s.corporations
            .get_mut(&corp_id)
            .unwrap()
            .set_division_balance(1, IskCents(1));
        s.items.insert(
            920_201,
            InventoryItemRecord {
                item_id: 920_201,
                type_id: 34,
                owner_id: corp_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::CorpSAG1,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        920_201
    });
    let mut corp_kw = HashMap::new();
    corp_kw.insert("useCorp".into(), PyRep::Bool(true));
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![entry_trit(34, corp_trit_id, 1_000_000.0, 1)])],
            &corp_kw,
        )
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    // Restore balances and test MaxOpenOrdersReached
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().balance = IskCents(5_000_000_000);
        s.corporations
            .get_mut(&corp_id)
            .unwrap()
            .set_division_balance(1, IskCents(5_000_000_000));
        // Seed 5 open orders (default max open orders with Trade 0 = 5)
        for i in 0..6_i64 {
            let oid = 9_200_500 + i;
            s.market_orders.insert(
                oid,
                MarketOrderRecord {
                    order_id: oid,
                    owner_id: char_id,
                    station_id: JITA_STATION_ID,
                    region_id: THE_FORGE_REGION_ID,
                    type_id: 34,
                    is_bid: false,
                    price: IskCents(1_000),
                    vol_entered: 1,
                    vol_remaining: 1,
                    min_volume: 1,
                    is_npc: false,
                    escrow: IskCents(0),
                },
            );
        }
    });
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![entry_trit(34, jita_trit_id, 1_000.0, 1)])],
            &kw,
        )
        .unwrap_err()
        .code,
        "MaxOpenOrdersReached"
    );

    // Clear open orders and test immediate sell crossing min_volume skip, insolvent corp bid refund,
    // corp buyer Deliveries item placement, and partial bid remaining!
    let (other_buyer, corp_buyer, small_min_bid, insolv_corp_bid, good_corp_bid) =
        (920_010, 98_920_010, 9_200_601, 9_200_602, 9_200_603);
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.retain(|oid, _| *oid == fort_oid || *oid < 9_200_500);
        let mut buyer_ch = s.characters.values().next().unwrap().clone();
        buyer_ch.character_id = other_buyer;
        buyer_ch.name = "OtherBuyer".into();
        buyer_ch.corporation_id = corp_buyer;
        buyer_ch.solar_system_id = 30_000_142;
        buyer_ch.station_id = Some(JITA_STATION_ID);
        buyer_ch.balance = IskCents(1_000_000);
        s.characters.insert(other_buyer, buyer_ch);

        let mut bc = CorporationRecord::new(corp_buyer, "BuyerCorp", "BUYC", other_buyer)
            .with_master_wallet(IskCents(1_000_000));
        bc.set_division_balance(3, IskCents(500)); // too small for insolv_corp_bid shortfall
        s.corporations.insert(corp_buyer, bc);

        // min_volume 100 > our sell qty 5 -> skipped!
        s.market_orders.insert(
            small_min_bid,
            MarketOrderRecord {
                order_id: small_min_bid,
                owner_id: other_buyer,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents(2_000),
                vol_entered: 200,
                vol_remaining: 200,
                min_volume: 100,
                is_npc: false,
                escrow: IskCents(400_000),
            },
        );
        // Insolvent corp bid: needs 100*1800 = 180_000; escrow 1_000; uncovered ~179_000 > div3 500 -> cancelled & escrow 1_000 refunded to div3!
        s.market_orders.insert(
            insolv_corp_bid,
            MarketOrderRecord {
                order_id: insolv_corp_bid,
                owner_id: corp_buyer,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents(1_800),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(1_000),
            },
        );
        s.market_order_meta.insert(
            insolv_corp_bid,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 90,
                range: "-1".into(),
                issued_by: Some(other_buyer),
                wallet_division: Some(3),
                is_corporation: true,
                state: None,
            },
        );
        // Solvent corp bid with vol_remaining 10 >= our sell qty 5 -> partially filled to Deliveries!
        s.market_orders.insert(
            good_corp_bid,
            MarketOrderRecord {
                order_id: good_corp_bid,
                owner_id: corp_buyer,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents(1_500),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(15_000),
            },
        );
    });
    let imm_res = call_escrow_kw(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![entry_trit(34, jita_trit_id, 14.0, 5)])],
        &kw,
    )
    .unwrap();
    assert_eq!(imm_res.dict_get("filledImmediately"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&small_min_bid].vol_remaining, 200);
        assert_eq!(s.market_orders[&insolv_corp_bid].vol_remaining, 0);
        assert_eq!(s.corporations[&corp_buyer].division_balance(3), IskCents(1_500));
        assert_eq!(s.market_orders[&good_corp_bid].vol_remaining, 5);
        assert!(s.items.values().any(|it| it.owner_id == corp_buyer && it.type_id == 34 && it.flag == InventoryFlag::Deliveries && it.quantity == 5));
    });

    // CancelCharOrder edge paths: unauthenticated (L862), CorporationNotFound (L893-896),
    // market_order_meta wallet_division fallback (L901-906), corp buy refund on div3 (L961),
    // create-new item stack when no existing stack at fort_id (L996-1016)!
    let mut unauth = sess.clone();
    unauth.character_id = None;
    assert!(call_escrow_kw(&ctx, &mut unauth, "CancelCharOrder", &[PyRep::Integer(fort_oid)], &kw).is_err());

    let missing_corp_buy = 9_200_700_i64;
    let div3_corp_buy = 9_200_701_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.insert(
            missing_corp_buy,
            MarketOrderRecord {
                order_id: missing_corp_buy,
                owner_id: 98_999_666,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents(100),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(100),
            },
        );
        s.market_orders.insert(
            div3_corp_buy,
            MarketOrderRecord {
                order_id: div3_corp_buy,
                owner_id: corp_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents(500),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(1_000),
            },
        );
        s.market_order_meta.insert(
            div3_corp_buy,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 90,
                range: "-1".into(),
                issued_by: Some(other_buyer), // differs from char_id so falls back to wallet_division (L901-906)
                wallet_division: Some(3),
                is_corporation: true,
                state: None,
            },
        );
        // Remove existing stack at online_fort_id so CancelCharOrder creates a new stack (L996-1016)
        s.items.retain(|_, it| !(it.location_id == online_fort_id && it.type_id == 34));
    });
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.corporation_id = 98_999_666;
        }
    });
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "CancelCharOrder", &[PyRep::Integer(missing_corp_buy)], &kw)
            .unwrap_err()
            .code,
        "CorporationNotFound"
    );
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
    });
    call_escrow_kw(&ctx, &mut sess, "CancelCharOrder", &[PyRep::Integer(div3_corp_buy)], &kw).unwrap();
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corporations[&corp_id].division_balance(3), IskCents(1_001_000));
    });
    call_escrow_kw(&ctx, &mut sess, "CancelCharOrder", &[PyRep::Integer(fort_oid)], &kw).unwrap();
    ctx.db.with_snapshot(|s| {
        assert!(s.items.values().any(|it| it.location_id == online_fort_id && it.owner_id == char_id && it.type_id == 34 && it.quantity == 10));
    });
}

#[test]
fn buy_multiple_items_and_history_cover_remote_procurement_jumps_upwell_and_overflow() {
    use eve_db::{CorporationRecord, TrainedSkillRecord};

    let ctx = test_ctx();
    let now = ctx.now_unix();
    let char_id = 930_001_i64;
    let seller_id = 930_002_i64;
    let corp_seller_id = 98_930_002_i64;
    let owner_corp_id = 98_930_003_i64;
    let online_fort_id = 10_000_930_001_i64;
    let offline_fort_id = 10_000_930_002_i64;
    let wrong_region_fort_id = 10_000_930_003_i64;

    // Pick a Forge station != Jita whose jump distance >= 2 from Jita (30_000_142) and one outside Forge
    let (remote_forge_st, remote_forge_sys) = ctx
        .sde
        .stations
        .values()
        .filter(|s| s.region_id == THE_FORGE_REGION_ID && s.solar_system_id != 30_000_142)
        .find_map(|s| {
            let d = super::super::solar_system_jump_distance(&ctx, 30_000_142, s.solar_system_id)?;
            if d >= 2 {
                Some((s.station_id, s.solar_system_id))
            } else {
                None
            }
        })
        .expect("remote Forge station with jump distance >= 2");
    let non_forge_st = ctx
        .sde
        .stations
        .values()
        .find(|s| s.region_id != THE_FORGE_REGION_ID)
        .expect("non-Forge station")
        .station_id;

    ctx.db.with_mut_snapshot(|s| {
        let base_char = s.characters.values().next().unwrap().clone();
        for cid in [char_id, seller_id] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.name = format!("Buyer-{cid}");
            ch.corporation_id = corp_seller_id;
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(JITA_STATION_ID);
            ch.balance = IskCents(10_000_000_000);
            s.characters.insert(cid, ch);
        }
        s.corporations.insert(
            corp_seller_id,
            CorporationRecord::new(corp_seller_id, "SellerCorp", "SELL", seller_id),
        );
        s.corporations.insert(
            owner_corp_id,
            CorporationRecord::new(owner_corp_id, "StructOwner", "STOW", seller_id),
        );
    });
    seed_upwell_market_structure(&ctx, online_fort_id, owner_corp_id, 30_000_142, true);
    seed_upwell_market_structure(&ctx, offline_fort_id, owner_corp_id, 30_000_142, false);
    seed_upwell_market_structure(&ctx, wrong_region_fort_id, owner_corp_id, 30_002_187, true);

    let mut sess = jita_session(char_id, corp_seller_id);
    let kw = HashMap::new();

    // 1. Undocked & empty-args / empty-list validation errors (L1051, L1069-1083)
    sess.station_id = None;
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[PyRep::List(vec![])], &kw)
            .unwrap_err()
            .code,
        "NotDockedAtStation"
    );
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[], &kw)
            .unwrap_err()
            .code,
        "NotDockedAtStation"
    );
    sess.station_id = Some(JITA_STATION_ID);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[PyRep::Integer(42)], &kw)
            .unwrap_err()
            .code,
        "InvalidBuyMultipleItems"
    );
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[PyRep::List(vec![])], &kw)
            .unwrap_err()
            .code,
        "InvalidBuyMultipleItems"
    );

    // Invalid entry shape, bad tid/qty/unpublished/negative maxPrice/bad orderRange (L1114-1149, L1191)
    for bad_entry in [
        PyRep::Integer(1),
        PyRep::Tuple(vec![PyRep::Integer(-1), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(i64::from(i32::MAX) + 1)]),
        PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(0)]),
        PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(1), PyRep::Real(-5.0)]),
        PyRep::keyval([
            ("typeID", PyRep::Integer(34)),
            ("quantity", PyRep::Integer(1)),
            ("orderRange", PyRep::Integer(-2)),
        ]),
    ] {
        assert_eq!(
            call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[PyRep::List(vec![bad_entry])], &kw)
                .unwrap_err()
                .code,
            "InvalidBuyMultipleItems"
        );
    }

    // Buyer station errors: offline structure, wrong-region structure, unknown station (L1161, L1167-1177)
    let valid_entry = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(1)])]);
    sess.station_id = Some(offline_fort_id);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[valid_entry.clone()], &kw)
            .unwrap_err()
            .code,
        "MarketServiceOffline"
    );
    sess.station_id = Some(wrong_region_fort_id);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[valid_entry.clone()], &kw)
            .unwrap_err()
            .code,
        "InvalidMarketStation"
    );
    sess.station_id = Some(69_999_998);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[valid_entry.clone()], &kw)
            .unwrap_err()
            .code,
        "InvalidMarketStation"
    );

    // 2. Remote entry station validation: offline, wrong region, InsufficientProcurementSkill (L1196, L1200, L1203-1209)
    sess.station_id = Some(JITA_STATION_ID);
    sess.region_id = Some(THE_FORGE_REGION_ID);
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "BuyMultipleItems",
            &[PyRep::List(vec![PyRep::keyval([
                ("typeID", PyRep::Integer(34)),
                ("quantity", PyRep::Integer(1)),
                ("stationID", PyRep::Integer(offline_fort_id)),
            ])])],
            &kw,
        )
        .unwrap_err()
        .code,
        "MarketServiceOffline"
    );
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "BuyMultipleItems",
            &[PyRep::List(vec![PyRep::keyval([
                ("typeID", PyRep::Integer(34)),
                ("quantity", PyRep::Integer(1)),
                ("stationID", PyRep::Integer(non_forge_st)),
            ])])],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidMarketStation"
    );
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "BuyMultipleItems",
            &[PyRep::List(vec![PyRep::keyval([
                ("typeID", PyRep::Integer(34)),
                ("quantity", PyRep::Integer(1)),
                ("stationID", PyRep::Integer(remote_forge_st)),
            ])])],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientProcurementSkill"
    );

    // Train Procurement (16594) level 5 + Visibility (3447) level 5 so remote station + jump range 40 succeeds!
    ctx.db.with_mut_snapshot(|s| {
        let sk = s.skills.entry(char_id).or_default();
        sk.insert(
            16594,
            TrainedSkillRecord {
                type_id: 16594,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        sk.insert(
            3447,
            TrainedSkillRecord {
                type_id: 3447,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    // Also test same-system remote station with Procurement level 1 (max_proc_jumps == 0, L1203)
    ctx.db.with_mut_snapshot(|s| {
        s.skills.get_mut(&char_id).unwrap().get_mut(&16594).unwrap().level = 1;
    });
    let _ = call_escrow_kw(
        &ctx,
        &mut sess,
        "BuyMultipleItems",
        &[PyRep::List(vec![PyRep::keyval([
            ("typeID", PyRep::Integer(34)),
            ("quantity", PyRep::Integer(1)),
            ("stationID", PyRep::Integer(online_fort_id)),
        ])])],
        &kw,
    )
    .unwrap();
    // Restore Procurement 5 so region-wide / jump-distance remote works (L1204)
    ctx.db.with_mut_snapshot(|s| {
        s.skills.get_mut(&char_id).unwrap().get_mut(&16594).unwrap().level = 5;
    });

    // 3. Seed:
    //    - Ask at offline_fort_id (skipped by order match filter L1244-1246)
    //    - Ask owned by corp_seller_id at remote_forge_st (matched with orderRange = 40 -> L1238, L1337, L1365)
    //    - Overflow ask for type 35 at 60% i64::MAX (triggers MarketOrderOverflow L1272-1274)
    //    - Total-cost overflow asks for type 36 (triggers MarketOrderOverflow L1277-1279)
    let _ = remote_forge_sys;
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.retain(|_, o| o.is_npc && o.type_id != 34 && o.type_id != 35 && o.type_id != 36);
        s.market_orders.insert(
            9_300_801,
            MarketOrderRecord {
                order_id: 9_300_801,
                owner_id: seller_id,
                station_id: offline_fort_id,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents(100), // cheapest, but offline hub
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        s.market_orders.insert(
            9_300_802,
            MarketOrderRecord {
                order_id: 9_300_802,
                owner_id: corp_seller_id,
                station_id: remote_forge_st,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents(500),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        s.market_orders.insert(
            9_300_810,
            MarketOrderRecord {
                order_id: 9_300_810,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 35,
                is_bid: false,
                price: IskCents(i64::MAX / 2),
                vol_entered: 4,
                vol_remaining: 4,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        s.market_orders.insert(
            9_300_811,
            MarketOrderRecord {
                order_id: 9_300_811,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 36,
                is_bid: false,
                price: IskCents(i64::MAX / 2 + 1),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        s.market_orders.insert(
            9_300_812,
            MarketOrderRecord {
                order_id: 9_300_812,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 36,
                is_bid: false,
                price: IskCents(i64::MAX / 2 + 10),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    // Jump-range 40 + 3-arg BuyMultipleItems returns PyRep::List (L1238, L1337, L1365, L1415)!
    let list_res = call_escrow_kw(
        &ctx,
        &mut sess,
        "BuyMultipleItems",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::List(vec![PyRep::keyval([
                ("typeID", PyRep::Integer(34)),
                ("quantity", PyRep::Integer(3)),
                ("stationID", PyRep::Integer(remote_forge_st)),
                ("orderRange", PyRep::Integer(40)),
            ])]),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap();
    assert!(matches!(list_res, PyRep::List(ref l) if l.len() == 1));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&9_300_801].vol_remaining, 10);
        assert_eq!(s.market_orders[&9_300_802].vol_remaining, 7);
        assert!(s.corporations[&corp_seller_id].master_wallet_balance().0 > 0);
    });

    // InsufficientISK when ask_price exceeds buyer balance (L1267, L1293)
    let ins_res = call_escrow_kw(
        &ctx,
        &mut sess,
        "BuyMultipleItems",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(35),
            PyRep::Integer(3),
        ])])],
        &kw,
    )
    .unwrap();
    let Some(PyRep::List(statuses)) = ins_res.dict_get("itemStatuses") else {
        panic!("expected itemStatuses list");
    };
    assert_eq!(
        statuses[0].dict_get("status"),
        Some(&PyRep::String("InsufficientISK".into()))
    );

    // GetOldPriceHistory NoRegion and missing typeID (L1439-1442, L1448-1451)
    sess.region_id = None;
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "GetOldPriceHistory", &[PyRep::Integer(34)], &kw)
            .unwrap_err()
            .code,
        "NoRegion"
    );
    sess.region_id = Some(THE_FORGE_REGION_ID);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "GetOldPriceHistory", &[], &kw)
            .unwrap_err()
            .code,
        "InvalidMarketOrder"
    );
    let _ = now;
}
