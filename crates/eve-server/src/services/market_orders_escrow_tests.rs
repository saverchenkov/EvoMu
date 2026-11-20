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
    assert_eq!(old_hist.row_count(), Some(0));

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
    assert_eq!(new_hist.row_count(), Some(1));

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

#[test]
fn sleeper_npc_buy_orders_never_deplete_and_plex_cross_region_escrow_paths() {
    use crate::services::market_contracts::{
        handle as handle_contracts, is_permanent_npc_sleeper_buy_order, SLEEPER_BLUE_LOOT_ORDERS,
    };

    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let buyer_id = 90_000_002_i64;
    let (corp_id, buyer_corp_id) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&char_id].corporation_id,
            s.characters[&buyer_id].corporation_id,
        )
    });
    const CONCORD_STATION_ID: i64 = 60_012_310; // Mastakomon V - CONCORD Bureau (The Forge)
    const CONCORD_SYSTEM_ID: u32 = 30_000_202;
    let mut sess = jita_session(char_id, corp_id);
    sess.station_id = Some(CONCORD_STATION_ID);
    sess.solar_system_id = Some(CONCORD_SYSTEM_ID);
    let kw = HashMap::new();

    // Seed permanent NPC buy orders at CONCORD/DED stations (NOT Jita) via StartupCheck.
    handle_contracts(&ctx, &mut sess, "marketProxy", "StartupCheck", &[], &kw)
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|s| {
        assert!(
            s.market_orders
                .values()
                .all(|o| !(o.station_id == JITA_STATION_ID && is_permanent_npc_sleeper_buy_order(o))),
            "Jita 4-4 must NOT carry permanent Sleeper NPC buy orders"
        );
    });

    // Remove Accounting (0 -> 7.5% sales tax) and clear competing market orders.
    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(char_id).or_default().remove(&16622);
        s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(100_000_000);
        s.characters.get_mut(&buyer_id).unwrap().balance = IskCents::from_isk(500_000_000);
        for &(tid, _) in SLEEPER_BLUE_LOOT_ORDERS {
            s.next_entity_id += 1;
            let iid = s.next_entity_id;
            s.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: CONCORD_STATION_ID,
                    flag: InventoryFlag::Hangar,
                    quantity: 10,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Seed a non-Sleeper NPC buy order to verify regular NPC orders still deplete normally.
        s.market_orders.insert(
            9_390_001,
            MarketOrderRecord {
                order_id: 9_390_001,
                owner_id: 1_000_035,
                station_id: CONCORD_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(10),
                vol_entered: 50,
                vol_remaining: 50,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        s.next_entity_id += 1;
        let trit_iid = s.next_entity_id;
        s.items.insert(
            trit_iid,
            InventoryItemRecord {
                item_id: trit_iid,
                type_id: 34,
                owner_id: char_id,
                location_id: CONCORD_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 15,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Sell all 4 Sleeper loot types twice without calling StartupCheck again.
    for round in 0..2 {
        let mut batch = Vec::new();
        let mut expected_gross_cents = 0_i64;
        for &(tid, price_isk) in SLEEPER_BLUE_LOOT_ORDERS {
            let qty = 2_i64;
            expected_gross_cents += price_isk * qty * 100;
            batch.push(PyRep::keyval([
                ("stationID", PyRep::Integer(CONCORD_STATION_ID)),
                ("typeID", PyRep::Integer(tid as i64)),
                ("itemID", PyRep::Integer(0)),
                ("price", PyRep::Real(price_isk as f64)),
                ("quantity", PyRep::Integer(qty)),
            ]));
        }
        let bal_before = ctx.db.get_character(char_id).unwrap().balance.0;
        let res = call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(batch),
                PyRep::Bool(false),
                PyRep::Integer(0),
                PyRep::Real(0.0),
            ],
            &kw,
        )
        .unwrap_or_else(|e| panic!("Sleeper multi-sell round {round} failed: {e:?}"));
        assert_eq!(res.dict_get("filledImmediately"), Some(&PyRep::Bool(true)));
        let expected_tax_cents = (expected_gross_cents * 75) / 1_000;
        let expected_net_cents = expected_gross_cents - expected_tax_cents;
        let reported_tax = res.dict_get("salesTaxISK").and_then(PyRep::as_f64).unwrap_or(0.0);
        assert!((reported_tax - (expected_tax_cents as f64) / 100.0).abs() < 0.01);
        let bal_after = ctx.db.get_character(char_id).unwrap().balance.0;
        assert_eq!(bal_after - bal_before, expected_net_cents);

        ctx.db.with_snapshot(|s| {
            for &(tid, _) in SLEEPER_BLUE_LOOT_ORDERS {
                let ord = s
                    .market_orders
                    .values()
                    .find(|o| is_permanent_npc_sleeper_buy_order(o) && o.station_id == CONCORD_STATION_ID && o.type_id == tid)
                    .expect("Permanent Sleeper NPC buy order must exist at CONCORD station");
                assert_eq!(
                    ord.vol_remaining, ord.vol_entered,
                    "Sleeper NPC buy order for {tid} depleted in round {round}"
                );
            }
        });
    }

    // Non-Sleeper NPC buy order for Tritanium (34) MUST still deplete normally.
    call_escrow_kw(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(CONCORD_STATION_ID)),
                ("typeID", PyRep::Integer(34)),
                ("itemID", PyRep::Integer(0)),
                ("price", PyRep::Real(10.0)),
                ("quantity", PyRep::Integer(15)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(0),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.market_orders[&9_390_001].vol_remaining), 35);

    // Move session back to Jita for subsequent PLEX tests.
    sess.station_id = Some(JITA_STATION_ID);
    sess.solar_system_id = Some(30_000_142);

    // Cross-region PLEX BuyMultipleItems + PlaceMultiSellOrder (from vault) + global GetOldPriceHistory/GetNewPriceHistory
    let remote_station = ctx
        .sde
        .stations
        .values()
        .find(|s| {
            s.region_id != 0
                && s.region_id != THE_FORGE_REGION_ID
                && ctx
                    .sde
                    .solar_systems
                    .get(&s.solar_system_id)
                    .is_some_and(|sys| sys.region_id == s.region_id)
        })
        .expect("Remote non-Forge NPC station in SDE")
        .clone();
    let other_region_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|sys| sys.region_id != THE_FORGE_REGION_ID && sys.region_id != remote_station.region_id)
        .map(|sys| sys.region_id)
        .unwrap_or(10_000_032);

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.retain(|_, o| o.type_id != PLEX_TYPE_ID);
        s.market_history.retain(|(_, tid), _| *tid != PLEX_TYPE_ID);
        s.market_transactions.retain(|_, tx| tx.type_id != PLEX_TYPE_ID);
        let v1 = s.plex_vault_balance(char_id);
        let _ = s.adjust_plex_vault(char_id, 20 - v1);
        let v2 = s.plex_vault_balance(buyer_id);
        let _ = s.adjust_plex_vault(buyer_id, -v2);
        s.market_orders.insert(
            9_390_002,
            MarketOrderRecord {
                order_id: 9_390_002,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents::from_isk(5_100_000),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        s.market_orders.insert(
            9_390_003,
            MarketOrderRecord {
                order_id: 9_390_003,
                owner_id: buyer_id,
                station_id: remote_station.station_id,
                region_id: remote_station.region_id,
                type_id: PLEX_TYPE_ID,
                is_bid: true,
                price: IskCents::from_isk(4_900_000),
                vol_entered: 4,
                vol_remaining: 4,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(19_600_000),
            },
        );
    });

    let mut remote_buyer_sess = SessionState::new(9399);
    remote_buyer_sess.user_id = Some(2);
    remote_buyer_sess.character_id = Some(buyer_id);
    remote_buyer_sess.corporation_id = Some(buyer_corp_id);
    remote_buyer_sess.station_id = Some(remote_station.station_id);
    remote_buyer_sess.solar_system_id = Some(remote_station.solar_system_id);
    remote_buyer_sess.region_id = Some(remote_station.region_id);

    // Buyer in remote region buys Jita PLEX ask via BuyMultipleItems -> PLEX goes to vault!
    let buy_res = call_escrow_kw(
        &ctx,
        &mut remote_buyer_sess,
        "BuyMultipleItems",
        &[
            PyRep::Integer(remote_station.station_id),
            PyRep::List(vec![PyRep::keyval([
                ("typeID", PyRep::Integer(PLEX_TYPE_ID as i64)),
                ("quantity", PyRep::Integer(3)),
                ("maxPrice", PyRep::Real(5_100_000.0)),
            ])]),
        ],
        &kw,
    )
    .expect("Cross-region BuyMultipleItems for PLEX");
    assert_eq!(buy_res.dict_get("totalQuantity"), Some(&PyRep::Integer(3)));
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(buyer_id)), 3);
    assert_eq!(ctx.db.with_snapshot(|s| s.market_orders[&9_390_002].vol_remaining), 7);

    // Seller in Jita sells 4 PLEX from PLEX vault via PlaceMultiSellOrder against remote buyer's bid!
    let multi_sell_plex = call_escrow_kw(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(PLEX_TYPE_ID as i64)),
                ("itemID", PyRep::Integer(0)),
                ("price", PyRep::Real(4_900_000.0)),
                ("quantity", PyRep::Integer(4)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(0),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .expect("Cross-region PlaceMultiSellOrder for PLEX from vault");
    assert_eq!(multi_sell_plex.dict_get("filledImmediately"), Some(&PyRep::Bool(true)));
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id)), 16);
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(buyer_id)), 7);
    assert_eq!(ctx.db.with_snapshot(|s| s.market_orders[&9_390_003].vol_remaining), 0);

    // Corporate cross-region PLEX PlaceMultiSellOrder must deliver PLEX to buyer's bid station (`Deliveries`), not seller's station.
    let corp_plex_bid_id = 9_390_005_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.entry(buyer_corp_id).or_insert_with(|| {
            let mut corp = eve_db::CorporationRecord::default();
            corp.corporation_id = buyer_corp_id;
            corp.ceo_id = buyer_id;
            corp.name = "RemotePlexCorp".into();
            corp.ticker = "RPLC".into();
            corp.set_division_balance(1, IskCents::from_isk(25_000_000));
            corp
        });
        s.market_orders.insert(
            corp_plex_bid_id,
            MarketOrderRecord {
                order_id: corp_plex_bid_id,
                owner_id: buyer_corp_id,
                station_id: remote_station.station_id,
                region_id: remote_station.region_id,
                type_id: PLEX_TYPE_ID,
                is_bid: true,
                price: IskCents::from_isk(5_000_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(10_000_000),
            },
        );
        crate::services::market_trading::save_market_order_meta(
            s,
            corp_plex_bid_id,
            crate::services::market_trading::MarketOrderMeta {
                range: 32_767,
                duration: 90,
                issue_unix: ctx.now_unix(),
                order_state: None,
                account_key: Some(1000),
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
    });
    let corp_multi_sell = call_escrow_kw(
        &ctx,
        &mut sess,
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(PLEX_TYPE_ID as i64)),
                ("itemID", PyRep::Integer(0)),
                ("price", PyRep::Real(5_000_000.0)),
                ("quantity", PyRep::Integer(2)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(0),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .expect("Cross-region PlaceMultiSellOrder for corp PLEX bid");
    assert_eq!(corp_multi_sell.dict_get("filledImmediately"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.plex_vault_balance(char_id), 14);
        assert_eq!(s.market_orders[&corp_plex_bid_id].vol_remaining, 0);
        let corp_plex = s
            .items
            .values()
            .find(|it| it.owner_id == buyer_corp_id && it.type_id == PLEX_TYPE_ID && it.flag == InventoryFlag::Deliveries)
            .expect("Corp cross-region PLEX delivery item");
        assert_eq!(
            corp_plex.location_id, remote_station.station_id,
            "Corp PLEX delivery must go to buyer station {}, not seller Jita",
            remote_station.station_id
        );
        assert_eq!(corp_plex.quantity, 2);
    });

    // GetNewPriceHistory(PLEX_TYPE_ID) aggregates today's PLEX trades globally (even from region=None);
    // GetOldPriceHistory(PLEX_TYPE_ID) only returns days strictly before today UTC (zero duplicate overlap).
    let yesterday_date = eve_db::ledger::unix_secs_to_date_str(ctx.now_unix().saturating_sub(86_400));
    for test_reg in [Some(other_region_id), None] {
        remote_buyer_sess.region_id = test_reg;
        let old_hist = call_escrow_kw(
            &ctx,
            &mut remote_buyer_sess,
            "GetOldPriceHistory",
            &[PyRep::Integer(PLEX_TYPE_ID as i64)],
            &kw,
        )
        .expect("GetOldPriceHistory for PLEX");
        assert!(old_hist.crowset_rows().expect("old rows").is_empty());

        let new_hist = call_escrow_kw(
            &ctx,
            &mut remote_buyer_sess,
            "GetNewPriceHistory",
            &[PyRep::Integer(PLEX_TYPE_ID as i64)],
            &kw,
        )
        .expect("GetNewPriceHistory for PLEX");
        let rows = new_hist.crowset_rows().expect("PLEX history rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].dbrow_get("lowPrice"), Some(&PyRep::Real(4_900_000.0)));
        assert_eq!(rows[0].dbrow_get("highPrice"), Some(&PyRep::Real(5_100_000.0)));
        assert_eq!(rows[0].dbrow_get("volume"), Some(&PyRep::Integer(9)));
        assert_eq!(rows[0].dbrow_get("orders"), Some(&PyRep::Integer(3)));
    }
    ctx.db.with_mut_snapshot(|s| {
        s.market_history
            .entry((THE_FORGE_REGION_ID, PLEX_TYPE_ID))
            .or_default()
            .push(eve_db::MarketHistoryDayRecord {
                date: yesterday_date.clone(),
                lowest: 4_800_000.0,
                highest: 4_850_000.0,
                average: 4_820_000.0,
                volume: 4,
                order_count: 1,
                open_price: None,
                close_price: None,
            });
    });
    remote_buyer_sess.region_id = None;
    let old_hist_yest = call_escrow_kw(
        &ctx,
        &mut remote_buyer_sess,
        "GetOldPriceHistory",
        &[PyRep::Integer(PLEX_TYPE_ID as i64)],
        &kw,
    )
    .expect("GetOldPriceHistory yesterday");
    let old_rows = old_hist_yest.crowset_rows().expect("old PLEX rows");
    assert_eq!(old_rows.len(), 1);
    assert_eq!(old_rows[0].dbrow_get("volume"), Some(&PyRep::Integer(4)));
    let new_hist_today = call_escrow_kw(
        &ctx,
        &mut remote_buyer_sess,
        "GetNewPriceHistory",
        &[PyRep::Integer(PLEX_TYPE_ID as i64)],
        &kw,
    )
    .expect("GetNewPriceHistory today");
    let new_rows = new_hist_today.crowset_rows().expect("new PLEX rows");
    assert_eq!(new_rows.len(), 1);
    assert_eq!(new_rows[0].dbrow_get("volume"), Some(&PyRep::Integer(9)));
    assert_ne!(
        old_rows[0].dbrow_get("historyDate"),
        new_rows[0].dbrow_get("historyDate")
    );

    // With active region_id, out-of-u32-range 64-bit IDs (positive or negative) must NOT
    // truncate modulo 2^32 to match PLEX_TYPE_ID or Tritanium (34).
    let mut active_reg_sess = jita_session(buyer_id, 1_000_009);
    for method in ["GetOldPriceHistory", "GetNewPriceHistory"] {
        for bogus_tid in [
            (1_i64 << 32) | (PLEX_TYPE_ID as i64),
            (1_i64 << 32) | 34_i64,
            (-1_i64 << 32) | 34_i64,
        ] {
            let res = call_escrow_kw(
                &ctx,
                &mut active_reg_sess,
                method,
                &[PyRep::Integer(bogus_tid)],
                &kw,
            )
            .expect("Out-of-range typeID should return empty CRowset rather than panic/truncate");
            assert!(
                res.crowset_rows().expect("CRowset").is_empty(),
                "{method} must return empty history for truncated 64-bit typeID {bogus_tid}"
            );
        }
    }

    // With region=None, non-PLEX types still return NoRegion without mutating DB state.
    let (orders_before, next_eid_before) =
        ctx.db.with_snapshot(|s| (s.market_orders.len(), s.next_entity_id));
    for method in ["GetOldPriceHistory", "GetNewPriceHistory"] {
        let err = call_escrow_kw(
            &ctx,
            &mut remote_buyer_sess,
            method,
            &[PyRep::Integer(34)],
            &kw,
        )
        .expect_err("Non-PLEX price history with region=None must fail with NoRegion");
        assert_eq!(err.code, "NoRegion");
    }
    let (orders_after, next_eid_after) =
        ctx.db.with_snapshot(|s| (s.market_orders.len(), s.next_entity_id));
    assert_eq!(orders_before, orders_after);
    assert_eq!(next_eid_before, next_eid_after);
}

#[test]
fn cancel_char_order_rejects_unowned_order_before_sweeping_expired_orders() {
    use crate::services::market_trading::{save_market_order_meta, MarketOrderMeta};
    use eve_db::{IskCents, MarketOrderRecord};

    let ctx = test_ctx();
    let owner_id = 90_000_001_i64;
    let attacker_id = 90_000_002_i64;
    let now = ctx.now_unix();
    let kw = HashMap::new();
    let (owned_oid, expired_oid) = (9_700_001_i64, 9_700_002_i64);

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.retain(|_, o| o.is_npc);
        s.market_orders.insert(
            owned_oid,
            MarketOrderRecord {
                order_id: owned_oid,
                owner_id,
                station_id: JITA_STATION_ID,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(10),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(100),
            },
        );
        save_market_order_meta(
            s,
            owned_oid,
            MarketOrderMeta {
                range: -1,
                duration: 90,
                issue_unix: now,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
        s.market_orders.insert(
            expired_oid,
            MarketOrderRecord {
                order_id: expired_oid,
                owner_id,
                station_id: JITA_STATION_ID,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(10),
                vol_entered: 5,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(50),
            },
        );
        save_market_order_meta(
            s,
            expired_oid,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: now.saturating_sub(2 * 86_400),
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
    });

    let attacker_corp_id = ctx.db.with_snapshot(|s| s.characters[&attacker_id].corporation_id);
    let mut attacker_sess = jita_session(attacker_id, attacker_corp_id);
    let eid_before = ctx.db.with_snapshot(|s| s.next_entity_id);
    let err = call_escrow_kw(
        &ctx,
        &mut attacker_sess,
        "CancelCharOrder",
        &[PyRep::Integer(owned_oid)],
        &kw,
    )
    .expect_err("Cancelling another character's order must fail");
    assert_eq!(err.code, "OrderOwnershipViolation");

    // Unowned cancel failure must NOT trigger a global expiry sweep.
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&expired_oid].vol_remaining, 5);
        assert_eq!(s.next_entity_id, eid_before);
    });
}

#[test]
fn rejected_multi_sell_and_buy_multiple_do_not_sweep_expired_orders() {
    use crate::services::market_trading::{save_market_order_meta, MarketOrderMeta};
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, AccessGroupRecord, StructureAccessProfile,
    };
    use crate::services::structure_profile_services::SETTING_MARKET_TAX;
    use eve_db::{CorporationRecord, IskCents, MarketOrderRecord};

    let ctx = test_ctx();
    let caller_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let now = ctx.now_unix();
    let kw = HashMap::new();
    let (caller_corp_id, expired_oid) = (98_930_001_i64, 9_710_001_i64);
    let struct_owner_corp = 98_930_002_i64;
    let offline_fort_id = 10_000_930_001_i64;
    let acl_denied_fort_id = 10_000_930_002_i64;

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.retain(|_, o| o.is_npc);
        s.items.retain(|_, i| i.owner_id != caller_id);
        s.corporations.insert(
            caller_corp_id,
            CorporationRecord::new(caller_corp_id, "CallerCorp", "CLRC", other_id),
        );
        s.corporations.insert(
            struct_owner_corp,
            CorporationRecord::new(struct_owner_corp, "StructOwnerCorp", "SOCP", other_id),
        );
        if let Some(ch) = s.characters.get_mut(&caller_id) {
            ch.corporation_id = caller_corp_id;
        }
        s.corp_role_assignments.remove(&(caller_corp_id, caller_id));
        s.market_orders.insert(
            expired_oid,
            MarketOrderRecord {
                order_id: expired_oid,
                owner_id: other_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(10),
                vol_entered: 5,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(50),
            },
        );
        save_market_order_meta(
            s,
            expired_oid,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: now.saturating_sub(2 * 86_400),
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
    });
    seed_upwell_market_structure(&ctx, offline_fort_id, struct_owner_corp, 30_000_142, false);
    seed_upwell_market_structure(&ctx, acl_denied_fort_id, struct_owner_corp, 30_000_142, true);
    ctx.db.with_mut_snapshot(|s| {
        let mut ext = load_ext_state(s);
        ext.owner_groups.insert(
            9301,
            AccessGroupRecord {
                group_id: 9301,
                creator_id: other_id,
                owner_corp_id: struct_owner_corp,
                name: "AllowedOnlyOther".into(),
                description: String::new(),
                members: HashMap::from([(other_id, 0)]),
            },
        );
        let mut prof = StructureAccessProfile {
            profile_id: 9301,
            corp_only: false,
            ..Default::default()
        };
        prof.service_acl_rules.insert(SETTING_MARKET_TAX, vec![(9301, 100)]);
        ext.access_profiles.insert(acl_denied_fort_id, prof);
        save_ext_state(s, &ext);
    });

    let _ = ctx.drain_notifications_for_char(other_id);
    let eid_before = ctx.db.with_snapshot(|s| s.next_entity_id);
    let mut sess = jita_session(caller_id, caller_corp_id);

    let sell_entry = |st_id: i64| {
        PyRep::List(vec![PyRep::keyval([
            ("stationID", PyRep::Integer(st_id)),
            ("typeID", PyRep::Integer(34)),
            ("itemID", PyRep::Integer(0)),
            ("price", PyRep::Real(10.0)),
            ("quantity", PyRep::Integer(5)),
        ])])
    };
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "PlaceMultiSellOrder", &[sell_entry(JITA_STATION_ID)], &kw)
            .unwrap_err()
            .code,
        "InsufficientInventoryForSellOrder"
    );
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut sess,
            "PlaceMultiSellOrder",
            &[sell_entry(JITA_STATION_ID), PyRep::Bool(true)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CorpRoleRequired"
    );
    sess.station_id = Some(offline_fort_id);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "PlaceMultiSellOrder", &[sell_entry(offline_fort_id)], &kw)
            .unwrap_err()
            .code,
        "MarketHubOffline"
    );
    sess.station_id = Some(acl_denied_fort_id);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "PlaceMultiSellOrder", &[sell_entry(acl_denied_fort_id)], &kw)
            .unwrap_err()
            .code,
        "StructureServiceAccessDenied"
    );

    let buy_entry = |st_id: i64, range: i64| {
        PyRep::List(vec![PyRep::keyval([
            ("typeID", PyRep::Integer(34)),
            ("quantity", PyRep::Integer(5)),
            ("price", PyRep::Real(10.0)),
            ("stationID", PyRep::Integer(st_id)),
            ("range", PyRep::Integer(range)),
        ])])
    };
    sess.station_id = Some(JITA_STATION_ID);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[buy_entry(JITA_STATION_ID, 32767)], &kw)
            .unwrap_err()
            .code,
        "InsufficientVisibilitySkill"
    );
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[buy_entry(JITA_STATION_ID, 99)], &kw)
            .unwrap_err()
            .code,
        "InvalidBuyMultipleItems"
    );
    sess.station_id = Some(offline_fort_id);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[buy_entry(offline_fort_id, -1)], &kw)
            .unwrap_err()
            .code,
        "MarketServiceOffline"
    );
    sess.station_id = Some(acl_denied_fort_id);
    assert_eq!(
        call_escrow_kw(&ctx, &mut sess, "BuyMultipleItems", &[buy_entry(acl_denied_fort_id, -1)], &kw)
            .unwrap_err()
            .code,
        "StructureServiceAccessDenied"
    );

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&expired_oid].vol_remaining, 5);
        assert_eq!(s.next_entity_id, eid_before);
    });
    assert!(ctx.drain_notifications_for_char(other_id).is_empty());
}

#[test]
fn place_multi_sell_unlocks_unswept_expired_sell_orders_and_enforces_upwell_plex_bid_acl() {
    use crate::services::market_trading::{save_market_order_meta, MarketOrderMeta};
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, AccessGroupRecord, StructureAccessProfile,
    };
    use crate::services::structure_profile_services::SETTING_MARKET_TAX;
    use eve_db::{
        corp_roles, CorpMemberRoleAssignment, CorporationRecord, IskCents, MarketOrderRecord,
    };

    let ctx = test_ctx();
    let seller_id = 90_000_001_i64;
    let deliv_trader_id = 90_000_002_i64;
    let restricted_trader_id = 90_000_003_i64;
    let bid_buyer_id = 90_000_004_i64;
    let corp_id = 98_950_001_i64;
    let struct_owner_corp = 98_950_002_i64;
    let acl_fort_id = 10_000_950_101_i64;
    let now = ctx.now_unix();
    let kw = HashMap::new();
    let p_trit_exp_oid = 9_750_001_i64;
    let p_plex_exp_oid = 9_750_002_i64;
    let c_trit_exp_oid = 9_750_003_i64;
    let acl_plex_bid_oid = 9_750_004_i64;

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.retain(|_, o| o.is_npc);
        s.items.clear();
        let template_ch = s.characters.values().next().unwrap().clone();
        for cid in [seller_id, deliv_trader_id, restricted_trader_id, bid_buyer_id] {
            let mut ch = template_ch.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = if cid == deliv_trader_id || cid == restricted_trader_id {
                corp_id
            } else {
                1_000_009
            };
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(JITA_STATION_ID);
            ch.balance = IskCents::from_isk(1_000_000_000);
            s.characters.insert(cid, ch);
            let cur_v = s.plex_vault_balance(cid);
            if cur_v != 0 {
                let _ = s.adjust_plex_vault(cid, -cur_v);
            }
        }
        // Give deliv_trader_id 2 PLEX in vault for later ACL-permitted crossing test.
        let _ = s.adjust_plex_vault(deliv_trader_id, 2);

        let mut corp = CorporationRecord::new(corp_id, "ExpiredSellCorp", "ESCP", deliv_trader_id)
            .with_master_wallet(IskCents::from_isk(1_000_000_000));
        corp.hq_station_id = Some(JITA_STATION_ID);
        s.corporations.insert(corp_id, corp);
        s.corporations.insert(
            struct_owner_corp,
            CorporationRecord::new(struct_owner_corp, "AclStructCorp", "ASCP", deliv_trader_id),
        );
        // restricted_trader_id has Trader + div 1 take, but NO Deliveries take and NO Director role.
        s.corp_role_assignments.insert(
            (corp_id, restricted_trader_id),
            CorpMemberRoleAssignment {
                character_id: restricted_trader_id,
                roles: corp_roles::ROLE_TRADER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0],
                grantable_roles: 0,
                roles_at_hq: 0,
                grantable_roles_at_hq: 0,
                roles_at_base: 0,
                grantable_roles_at_base: 0,
                roles_at_other: 0,
                grantable_roles_at_other: 0,
            },
        );
        // deliv_trader_id has Trader + div 1 take + Deliveries take.
        s.corp_role_assignments.insert(
            (corp_id, deliv_trader_id),
            CorpMemberRoleAssignment {
                character_id: deliv_trader_id,
                roles: corp_roles::ROLE_TRADER
                    | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]
                    | corp_roles::ROLE_DELIVERIES_HANGAR_TAKE,
                grantable_roles: 0,
                roles_at_hq: 0,
                grantable_roles_at_hq: 0,
                roles_at_base: 0,
                grantable_roles_at_base: 0,
                roles_at_other: 0,
                grantable_roles_at_other: 0,
            },
        );

        for (oid, owner_id, st_id, reg_id, tid, is_bid, price_isk, qty, dur, issue_unix) in [
            (
                p_trit_exp_oid,
                seller_id,
                JITA_STATION_ID,
                THE_FORGE_REGION_ID,
                34_u32,
                false,
                20.0_f64,
                10_i32,
                1_i64,
                now.saturating_sub(2 * 86_400),
            ),
            (
                p_plex_exp_oid,
                seller_id,
                60_008_494,
                10_000_043,
                PLEX_TYPE_ID,
                false,
                5_200_000.0,
                4,
                1,
                now.saturating_sub(2 * 86_400),
            ),
            (
                c_trit_exp_oid,
                corp_id,
                JITA_STATION_ID,
                THE_FORGE_REGION_ID,
                34,
                false,
                25.0,
                6,
                1,
                now.saturating_sub(2 * 86_400),
            ),
            (
                acl_plex_bid_oid,
                bid_buyer_id,
                acl_fort_id,
                THE_FORGE_REGION_ID,
                PLEX_TYPE_ID,
                true,
                4_900_000.0,
                10,
                90,
                now,
            ),
        ] {
            let p = IskCents::from_f64_isk(price_isk);
            s.market_orders.insert(
                oid,
                MarketOrderRecord {
                    order_id: oid,
                    owner_id,
                    station_id: st_id,
                    region_id: reg_id,
                    type_id: tid,
                    is_bid,
                    price: p,
                    vol_entered: qty,
                    vol_remaining: qty,
                    min_volume: 1,
                    is_npc: false,
                    escrow: if is_bid {
                        IskCents(p.0 * qty as i64)
                    } else {
                        IskCents(0)
                    },
                },
            );
            save_market_order_meta(
                s,
                oid,
                MarketOrderMeta {
                    range: -1,
                    duration: dur,
                    issue_unix,
                    order_state: None,
                    account_key: if owner_id == corp_id { Some(1000) } else { None },
                    final_vol_remaining: None,
                    closed_unix: None,
                },
            );
        }
    });

    seed_upwell_market_structure(&ctx, acl_fort_id, struct_owner_corp, 30_000_142, true);
    ctx.db.with_mut_snapshot(|s| {
        let mut ext = load_ext_state(s);
        ext.owner_groups.insert(
            9201,
            AccessGroupRecord {
                group_id: 9201,
                creator_id: deliv_trader_id,
                owner_corp_id: struct_owner_corp,
                name: "AllowedPlexCrossers".into(),
                description: String::new(),
                members: HashMap::from([(deliv_trader_id, 0)]),
            },
        );
        let mut prof = StructureAccessProfile {
            profile_id: 9201,
            corp_only: false,
            ..Default::default()
        };
        prof.service_acl_rules.insert(SETTING_MARKET_TAX, vec![(9201, 100)]);
        ext.access_profiles.insert(acl_fort_id, prof);
        save_ext_state(s, &ext);
    });

    let mut personal_sess = jita_session(seller_id, 1_000_009);
    // Asking for 11 Tritanium (exceeds 10 in expired order) must fail pre-sweep without sweeping.
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut personal_sess,
            "PlaceMultiSellOrder",
            &[PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(34)),
                ("itemID", PyRep::Integer(0)),
                ("price", PyRep::Real(20.0)),
                ("quantity", PyRep::Integer(11)),
            ])])],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientInventoryForSellOrder"
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&p_trit_exp_oid].vol_remaining, 10);
        assert_eq!(s.market_orders[&p_plex_exp_oid].vol_remaining, 4);
        assert_eq!(s.market_orders[&c_trit_exp_oid].vol_remaining, 6);
    });

    // Personal seller re-listing exact 10 Tritanium + 4 PLEX from unswept expired sell orders
    // must succeed AND must NOT cross the ACL-denied Upwell PLEX bid (`acl_plex_bid_oid`).
    let _ = call_escrow_kw(
        &ctx,
        &mut personal_sess,
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![
            PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(34)),
                ("itemID", PyRep::Integer(0)),
                ("price", PyRep::Real(20.0)),
                ("quantity", PyRep::Integer(10)),
            ]),
            PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(PLEX_TYPE_ID as i64)),
                ("itemID", PyRep::Integer(0)),
                ("price", PyRep::Real(4_800_000.0)),
                ("quantity", PyRep::Integer(4)),
            ]),
        ])],
        &kw,
    )
    .expect("PlaceMultiSellOrder re-lists personal Tritanium and PLEX from expired sell orders");

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&p_trit_exp_oid].vol_remaining, 0);
        assert_eq!(s.market_orders[&p_plex_exp_oid].vol_remaining, 0);
        assert_eq!(s.market_orders[&acl_plex_bid_oid].vol_remaining, 10);
        assert_eq!(s.plex_vault_balance(seller_id), 0);
        assert!(s.market_orders.values().any(|o| {
            o.owner_id == seller_id && o.type_id == 34 && !o.is_bid && o.vol_remaining == 10
        }));
        assert!(s.market_orders.values().any(|o| {
            o.owner_id == seller_id && o.type_id == PLEX_TYPE_ID && !o.is_bid && o.vol_remaining == 4
        }));
        // Corp order had not been swept yet before personal sweep? No, global sweep ran on personal re-list,
        // wait: verify corp re-list behavior with its own fresh expired corp order below!
    });

    // Re-seed an unswept expired corp sell order so restricted_trader_id vs deliv_trader_id are isolated.
    let c_trit_exp_oid_2 = 9_750_005_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.retain(|_, i| !(i.owner_id == corp_id && i.type_id == 34));
        s.market_orders.insert(
            c_trit_exp_oid_2,
            MarketOrderRecord {
                order_id: c_trit_exp_oid_2,
                owner_id: corp_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(25),
                vol_entered: 6,
                vol_remaining: 6,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            c_trit_exp_oid_2,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: now.saturating_sub(2 * 86_400),
                order_state: None,
                account_key: Some(1000),
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
    });

    let mut restricted_corp_sess = jita_session(restricted_trader_id, corp_id);
    // Trader without ROLE_DELIVERIES_HANGAR_TAKE cannot take swept deliveries, so must fail pre-sweep.
    assert_eq!(
        call_escrow_kw(
            &ctx,
            &mut restricted_corp_sess,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(34)),
                    ("itemID", PyRep::Integer(0)),
                    ("price", PyRep::Real(25.0)),
                    ("quantity", PyRep::Integer(6)),
                ])]),
                PyRep::Bool(true),
            ],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientInventoryForSellOrder"
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&c_trit_exp_oid_2].vol_remaining, 6);
    });

    // Trader with ROLE_DELIVERIES_HANGAR_TAKE succeeds, AND also crosses acl_plex_bid_oid (permitted in 9201).
    let mut deliv_sess = jita_session(deliv_trader_id, corp_id);
    let _ = call_escrow_kw(
        &ctx,
        &mut deliv_sess,
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(34)),
                ("itemID", PyRep::Integer(0)),
                ("price", PyRep::Real(25.0)),
                ("quantity", PyRep::Integer(6)),
            ])]),
            PyRep::Bool(true),
        ],
        &kw,
    )
    .expect("Corp trader with DELIVERIES take re-lists from unswept expired corp sell order");

    let _ = call_escrow_kw(
        &ctx,
        &mut deliv_sess,
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![PyRep::keyval([
            ("stationID", PyRep::Integer(JITA_STATION_ID)),
            ("typeID", PyRep::Integer(PLEX_TYPE_ID as i64)),
            ("itemID", PyRep::Integer(0)),
            ("price", PyRep::Real(4_800_000.0)),
            ("quantity", PyRep::Integer(2)),
        ])])],
        &kw,
    )
    .expect("ACL-permitted PlaceMultiSellOrder fills remote Upwell PLEX bid");

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&c_trit_exp_oid_2].vol_remaining, 0);
        assert!(s.market_orders.values().any(|o| {
            o.owner_id == corp_id && o.type_id == 34 && !o.is_bid && o.vol_remaining == 6
        }));
        assert_eq!(s.market_orders[&acl_plex_bid_oid].vol_remaining, 8);
    });
}
