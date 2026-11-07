//! Deep branch-coverage and mutation-killing unit tests for `market_trading` and `market_contracts`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::market_contracts::{
    handle as handle_contracts, JITA_STATION_ID, PLEX_TYPE_ID, THE_FORGE_REGION_ID,
};
use crate::services::market_trading::{
    best_asks_to_summary_rows, handle as handle_trading, load_market_order_meta,
    resolve_structure_broker_rate, save_market_order_meta, structure_owner_broker_share,
    MarketOrderMeta,
};
use eve_db::{
    corp_roles, ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    MarketOrderRecord, OwnerNoteRecord, TrainedSkillRecord, WalletJournalEntry,
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
    let mut s = SessionState::new(9501);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(JITA_STATION_ID);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(THE_FORGE_REGION_ID);
    s
}

fn mk_order(order_id: i64, owner_id: i64, type_id: u32, is_bid: bool, price_isk: i64, qty: i32, is_npc: bool, escrow_isk: i64) -> MarketOrderRecord {
    MarketOrderRecord {
        order_id, owner_id, station_id: JITA_STATION_ID, region_id: THE_FORGE_REGION_ID, type_id,
        is_bid, price: IskCents::from_isk(price_isk), vol_entered: qty, vol_remaining: qty,
        min_volume: 1, is_npc, escrow: IskCents::from_isk(escrow_isk),
    }
}

#[test]
fn meta_fallbacks_structure_broker_share_and_best_asks_summary_cap() {
    let ctx = test_ctx();
    let struct_id = 1_045_000_100_i64;
    let now = 1_760_000_000_u64;

    ctx.db.with_mut_snapshot(|s| {
        for (oid, state_str, range_str, expected_state, expected_range) in [
            (950_001_i64, Some("expired"), "station", Some(4), -1),
            (950_002_i64, Some("cancelled"), "solarsystem", Some(3), 0),
            (950_003_i64, None, "region", None, 32767),
            (950_004_i64, None, "10", None, 10),
        ] {
            let ord = mk_order(oid, 90_000_001, 34, true, 10, 10, false, 100);
            s.market_order_meta.insert(
                oid,
                eve_db::MarketOrderMeta {
                    issued_unix: now - 50, duration_days: 30, range: range_str.to_string(),
                    issued_by: Some(90_000_001), wallet_division: Some(3), is_corporation: true,
                    state: state_str.map(str::to_string),
                },
            );
            let m = load_market_order_meta(s, &ord, now);
            assert_eq!(m.order_state, expected_state);
            assert_eq!(m.range, expected_range);
            assert_eq!(m.account_key, Some(1002));
        }

        let npc_ord = mk_order(950_009, 1_000_035, 34, true, 5, 100, true, 0);
        let npc_m = load_market_order_meta(s, &npc_ord, now);
        assert_eq!(npc_m.range, 32767);
        assert_eq!(npc_m.duration, 365);

        let nid = -(1_000_000_000_000_i64.saturating_add(struct_id.abs()));
        s.owner_notes.insert(
            nid,
            OwnerNoteRecord { note_id: nid, character_id: -1, item_id: struct_id, label: "tax".into(), note: r#"{"0": 0.025}"#.into(), timestamp_unix: now },
        );
        assert!((resolve_structure_broker_rate(s, struct_id) - 0.030).abs() < 1e-9);
        assert_eq!(structure_owner_broker_share(s, struct_id, IskCents::from_isk(3000)), IskCents::from_isk(2500));
        s.owner_notes.get_mut(&nid).unwrap().note = r#"{"0": 0.0}"#.into();
        assert_eq!(structure_owner_broker_share(s, struct_id, IskCents::from_isk(3000)), IskCents(0));
        s.owner_notes.remove(&nid);
        assert_eq!(structure_owner_broker_share(s, struct_id, IskCents::from_isk(2000)), IskCents::from_isk(1000));
    });

    let mut many_orders: Vec<MarketOrderRecord> = (1..=3_505_u32)
        .map(|tid| mk_order(tid as i64, if tid <= 10 { 1_000_035 } else { 90_000_001 }, tid, false, 100, 10, false, 0))
        .collect();
    many_orders.push(mk_order(99_999, 90_000_001, 20, false, 50, 5, false, 0));
    let summary = best_asks_to_summary_rows(many_orders.iter());
    assert_eq!(summary.len(), 3_500);
    let row_20 = summary.iter().find(|r| r[2].as_i64() == Some(20)).expect("type 20 present");
    assert_eq!(row_20[0].as_f64(), Some(50.0));
}

#[test]
fn market_group_types_and_place_buy_order_corporate_and_error_branches() {
    let ctx = test_ctx();
    let buyer_id = 90_000_001_i64;
    let seller_corp_id = 98_500_001_i64;
    let buyer_corp_id = 98_500_002_i64;
    let mut sess = jita_session(buyer_id, buyer_corp_id);
    let kw = HashMap::new();

    // GetOrders, GetStationAsks, GetRegionBest error branches (missing typeID, NoRegion, NotDockedAtStation)
    assert_eq!(handle_trading(&ctx, &mut sess, "marketProxy", "GetOrders", &[], &kw).unwrap().unwrap_err().code, "InvalidMarketOrder");
    sess.region_id = None;
    assert_eq!(handle_trading(&ctx, &mut sess, "marketProxy", "GetOrders", &[PyRep::Integer(34)], &kw).unwrap().unwrap_err().code, "NoRegion");
    assert_eq!(handle_trading(&ctx, &mut sess, "marketProxy", "GetStationAsks", &[], &kw).unwrap().unwrap_err().code, "NoRegion");
    assert_eq!(handle_trading(&ctx, &mut sess, "marketProxy", "GetRegionBest", &[], &kw).unwrap().unwrap_err().code, "NoRegion");
    sess.region_id = Some(THE_FORGE_REGION_ID);
    sess.station_id = None;
    assert_eq!(handle_trading(&ctx, &mut sess, "marketProxy", "GetStationAsks", &[], &kw).unwrap().unwrap_err().code, "NotDockedAtStation");
    sess.station_id = Some(JITA_STATION_ID);

    for (args, expected_code) in [
        (vec![PyRep::Integer(JITA_STATION_ID)], "InvalidMarketOrder"),
        (vec![PyRep::Integer(JITA_STATION_ID), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(5), PyRep::Integer(99), PyRep::Integer(1), PyRep::Integer(90)], "InvalidMarketOrder"),
        (vec![PyRep::Integer(JITA_STATION_ID + 1), PyRep::Integer(34), PyRep::Real(10.0), PyRep::Integer(5), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90)], "StationMismatch"),
    ] {
        assert_eq!(handle_trading(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &args, &kw).unwrap().unwrap_err().code, expected_code);
    }

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&buyer_id) {
            ch.corporation_id = buyer_corp_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        s.corporations.insert(
            buyer_corp_id,
            CorporationRecord { corporation_id: buyer_corp_id, name: "BuyerCorp".into(), ticker: "BCRP".into(), ceo_id: 90_000_099, master_wallet: IskCents::from_isk(1_000_000), members: HashSet::from([buyer_id]), ..Default::default() },
        );
        s.corporations.insert(
            seller_corp_id,
            CorporationRecord { corporation_id: seller_corp_id, name: "SellerCorp".into(), ticker: "SCRP".into(), ceo_id: 90_000_098, master_wallet: IskCents(0), ..Default::default() },
        );
        s.market_orders.insert(951_001, mk_order(951_001, seller_corp_id, 34, false, 20, 10, false, 0));
    });

    assert_eq!(
        handle_trading(
            &ctx, &mut sess, "marketProxy", "PlaceBuyOrder",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(34), PyRep::Real(25.0), PyRep::Integer(50), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(0)], &kw,
        ).unwrap().unwrap_err().code,
        "ImmediateOrderUnfilled"
    );

    assert_eq!(
        handle_trading(
            &ctx, &mut sess, "marketProxy", "PlaceBuyOrder",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(34), PyRep::Real(25.0), PyRep::Integer(20), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(true), PyRep::Integer(1000)], &kw,
        ).unwrap().unwrap_err().code,
        "CorpRoleRequired"
    );

    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&buyer_corp_id).unwrap().member_roles.insert(buyer_id, corp_roles::ROLE_TRADER);
    });
    assert_eq!(
        handle_trading(
            &ctx, &mut sess, "marketProxy", "PlaceBuyOrder",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(34), PyRep::Real(25.0), PyRep::Integer(20), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(true), PyRep::Integer(1000)], &kw,
        ).unwrap().unwrap_err().code,
        "CorpRoleRequired"
    );

    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&buyer_corp_id).unwrap().member_roles.insert(
            buyer_id,
            corp_roles::ROLE_TRADER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0],
        );
    });
    let corp_buy = handle_trading(
        &ctx, &mut sess, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(34), PyRep::Real(25.0), PyRep::Integer(20), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(true), PyRep::Integer(1000)], &kw,
    ).unwrap().unwrap();
    let new_oid = corp_buy.dict_get("orderID").and_then(PyRep::as_i64).unwrap();
    assert_eq!(corp_buy.dict_get("escrowISK").and_then(PyRep::as_f64), Some(250.0));

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&951_001].vol_remaining, 0);
        assert_eq!(s.market_orders[&new_oid].owner_id, buyer_corp_id);
        assert_eq!(s.market_orders[&new_oid].vol_remaining, 10);
        assert!(s.corporations[&seller_corp_id].master_wallet.0 > 0);
    });
}

#[test]
fn modify_char_order_remote_daytrading_visibility_and_corporate_escrow_refund() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = 98_500_010_i64;
    let mut sess = jita_session(char_id, corp_id);
    let now = ctx.now_unix();

    let jita_const = ctx.sde.solar_systems[&30_000_142].constellation_id;
    let same_const_sys = ctx.sde.solar_systems.values().find(|s| s.solar_system_id != 30_000_142 && s.constellation_id == jita_const).map(|s| s.solar_system_id).unwrap_or(30_000_144);
    let diff_const_sys = ctx.sde.solar_systems.values().find(|s| s.region_id == THE_FORGE_REGION_ID && s.constellation_id != jita_const).map(|s| s.solar_system_id).unwrap_or(30_000_140);

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        let mut m_corp = CorporationRecord {
            corporation_id: corp_id, name: "ModCorp".into(), ticker: "MDCP".into(), ceo_id: char_id,
            master_wallet: IskCents::from_isk(500_000), members: HashSet::from([char_id]), ..Default::default()
        };
        m_corp.ensure_wallet_divisions();
        m_corp.wallet_divisions[0].balance = IskCents::from_isk(500_000);
        s.corporations.insert(corp_id, m_corp);
        s.market_orders.insert(952_001, mk_order(952_001, corp_id, 34, true, 100, 1000, false, 100_000));
        save_market_order_meta(
            s, 952_001,
            MarketOrderMeta { range: -1, duration: 90, issue_unix: now.saturating_sub(600), order_state: None, account_key: Some(1000), final_vol_remaining: None, closed_unix: None },
        );
    });

    sess.station_id = None;
    let kw = HashMap::new();
    assert_eq!(handle_contracts(&ctx, &mut sess, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(952_001), PyRep::Real(50.0)], &kw).unwrap().unwrap_err().code, "DaytradingSkillRequired");

    ctx.db.with_mut_snapshot(|s| {
        let sk = s.skills.entry(char_id).or_default();
        sk.insert(16595, TrainedSkillRecord { type_id: 16595, level: 2, skill_points: 1415.0 });
        sk.insert(3447, TrainedSkillRecord { type_id: 3447, level: 1, skill_points: 250.0 });
    });

    sess.solar_system_id = Some(diff_const_sys);
    assert_eq!(handle_contracts(&ctx, &mut sess, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(952_001), PyRep::Real(50.0)], &kw).unwrap().unwrap_err().code, "DaytradingSkillRequired");

    sess.solar_system_id = Some(same_const_sys);
    let mut kw_range = HashMap::new();
    kw_range.insert("newRange".to_string(), PyRep::Integer(5));
    assert_eq!(handle_contracts(&ctx, &mut sess, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(952_001), PyRep::Real(50.0)], &kw_range).unwrap().unwrap_err().code, "VisibilitySkillRequired");

    kw_range.insert("newRange".to_string(), PyRep::Integer(0));
    let mod_ok = handle_contracts(&ctx, &mut sess, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(952_001), PyRep::Real(50.0)], &kw_range).unwrap().unwrap();
    assert_eq!(mod_ok.dict_get("newPrice").and_then(PyRep::as_f64), Some(50.0));
    assert_eq!(mod_ok.dict_get("range").and_then(PyRep::as_i64), Some(0));
    ctx.db.with_snapshot(|s| {
        assert!(s.corporations[&corp_id].master_wallet > IskCents::from_isk(540_000));
    });
}

#[test]
fn plex_sell_hangar_margin_cover_order_history_inference_and_corp_auction_finish() {
    let ctx = test_ctx();
    let seller_id = 90_000_001_i64;
    let buyer_id = 90_000_002_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&seller_id].corporation_id);
    let mut sess = jita_session(seller_id, corp_id);
    let kw = HashMap::new();
    let now = ctx.now_unix();

    let (auction_cid, auction_item_id, courier_cid, courier_box_id) = ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        if let Some(ch) = s.characters.get_mut(&seller_id) {
            ch.balance = IskCents::from_isk(50_000_000);
        }
        let cur_vault = s.plex_vault_balance(seller_id);
        let _ = s.adjust_plex_vault(seller_id, -cur_vault);

        let mut buyer = s.characters[&seller_id].clone();
        buyer.character_id = buyer_id;
        buyer.user_id = 2;
        buyer.balance = IskCents::from_isk(20_000_000);
        s.characters.insert(buyer_id, buyer);

        s.items.insert(
            953_001,
            InventoryItemRecord { item_id: 953_001, type_id: PLEX_TYPE_ID, owner_id: seller_id, location_id: JITA_STATION_ID, flag: InventoryFlag::Hangar, quantity: 5, singleton: false, version: 1, ..Default::default() },
        );
        let mut plex_bid = mk_order(953_101, buyer_id, PLEX_TYPE_ID, true, 4_000_000, 2, false, 1_000_000);
        plex_bid.region_id = 10_000_043;
        s.market_orders.insert(953_101, plex_bid);

        for (oid, closed_ts) in [
            (953_201_i64, now.saturating_sub(100)),
            (953_202_i64, now.saturating_sub(100)),
            (953_203_i64, now.saturating_sub(100 * 86_400)),
        ] {
            let mut closed_ord = mk_order(oid, seller_id, 34, true, 10, 10, false, 0);
            closed_ord.vol_remaining = 0;
            s.market_orders.insert(oid, closed_ord);
            save_market_order_meta(
                s, oid,
                MarketOrderMeta { range: -1, duration: 90, issue_unix: closed_ts.saturating_sub(1000), order_state: None, account_key: None, final_vol_remaining: Some(10), closed_unix: Some(closed_ts) },
            );
        }
        s.wallet_journal.push(WalletJournalEntry { entry_id: 953_901, character_id: seller_id, entry_type: "MarketOrderCancelRefund".into(), amount: IskCents::from_isk(100), balance_after: IskCents::from_isk(100), reason: "Refunded escrow for cancelled buy order 953201".into() });
        s.wallet_journal.push(WalletJournalEntry { entry_id: 953_902, character_id: seller_id, entry_type: "MarketOrderExpireRefund".into(), amount: IskCents::from_isk(100), balance_after: IskCents::from_isk(200), reason: "Refunded escrow for expired buy order 953202".into() });

        let (a_item, a_cid, c_box, c_item, c_cid) = (953_301, 953_401, 953_302, 953_303, 953_402);
        s.items.insert(a_item, InventoryItemRecord { item_id: a_item, type_id: 587, owner_id: seller_id, location_id: JITA_STATION_ID, flag: InventoryFlag::Locked, quantity: 1, singleton: true, version: 1, ..Default::default() });
        s.contracts.insert(
            a_cid,
            ContractRecord { contract_id: a_cid, issuer_id: seller_id, issuer_corp_id: corp_id, for_corp: true, contract_type: "Auction".into(), status: "Outstanding".into(), start_station_id: JITA_STATION_ID, end_station_id: JITA_STATION_ID, item_ids: vec![a_item], expiration_unix: now.saturating_sub(10), ..Default::default() },
        );
        s.items.insert(c_item, InventoryItemRecord { item_id: c_item, type_id: 34, owner_id: seller_id, location_id: c_box, flag: InventoryFlag::Hangar, quantity: 10, singleton: false, version: 1, ..Default::default() });
        s.contracts.insert(
            c_cid,
            ContractRecord { contract_id: c_cid, issuer_id: seller_id, contract_type: "Courier".into(), status: "InProgress".into(), item_ids: vec![c_item], ..Default::default() },
        );
        (a_cid, a_item, c_cid, c_box)
    });

    let plex_kv = PyRep::keyval([("price", PyRep::Real(3_500_000.0)), ("quantity", PyRep::Integer(5)), ("duration", PyRep::Integer(90))]);
    let plex_res = handle_contracts(&ctx, &mut sess, "marketProxy", "PlacePlexSellOrder", &[plex_kv], &kw).unwrap().unwrap();
    assert_eq!(plex_res.dict_get("filledImmediately").and_then(PyRep::as_bool), Some(false));

    let hist = handle_contracts(&ctx, &mut sess, "marketProxy", "GetMarketOrderHistory", &[PyRep::Bool(false)], &kw).unwrap().unwrap();
    assert_eq!(hist.row_count(), Some(2));

    assert_eq!(handle_contracts(&ctx, &mut sess, "contractProxy", "FinishAuction", &[PyRep::Integer(auction_cid)], &kw).unwrap().unwrap(), PyRep::Bool(true));
    let courier_found = handle_contracts(&ctx, &mut sess, "contractProxy", "GetCourierContractFromItemID", &[PyRep::Integer(courier_box_id)], &kw).unwrap().unwrap();
    assert_eq!(courier_found.dict_get("contractID").and_then(PyRep::as_i64), Some(courier_cid));

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&auction_item_id].owner_id, corp_id);
        assert_eq!(s.items[&auction_item_id].flag, InventoryFlag::CorpSAG1);
    });
}
