//! Additional unit and mutation-killing tests for `market_contracts`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::market_contracts::*;
use crate::services::market_trading::{load_market_order_meta, save_market_order_meta, MarketOrderMeta};
use eve_db::{
    corp_roles, ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    MarketOrderRecord, TrainedSkillRecord, WalletJournalEntry,
};
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
    let mut s = SessionState::new(9201);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(JITA_STATION_ID);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(THE_FORGE_REGION_ID);
    s
}

#[test]
fn fee_math_boundary_levels_and_sig_figs_kill_mutants() {
    for valid in [0.01, 99.99, 100.0, 9999.0] {
        assert!(is_valid_4_sig_fig_price(valid));
    }
    for invalid in [100.01, 0.004, 1.005, f64::NEG_INFINITY] {
        assert!(!is_valid_4_sig_fig_price(invalid));
    }

    let gross = IskCents(100_000_00);
    for (lvl, expected) in [
        (0, 7_500_00), (1, 6_675_00), (2, 5_850_00),
        (3, 5_025_00), (4, 4_200_00), (5, 3_375_00), (6, 3_375_00),
    ] {
        assert_eq!(compute_sales_tax_cents(gross, lvl), IskCents(expected));
    }

    assert_eq!(
        compute_relist_fee_cents(IskCents::from_isk(5_000), IskCents::from_isk(10_000), -5, 0.03, 0),
        IskCents(10_000)
    );
    let p = IskCents::from_isk(10_000);
    assert_eq!(compute_relist_fee_cents(p, p, 100, 0.02, 0), IskCents::from_isk(10_000));
    assert_eq!(compute_relist_fee_cents(p, p, 100, 0.02, 3), IskCents::from_isk(6_400));
    assert_eq!(compute_relist_fee_cents(p, p, 100, 0.02, 5), IskCents::from_isk(4_000));
    assert_eq!(compute_relist_fee_cents(p, p, 100, 0.02, 6), IskCents::from_isk(4_000));
}

#[test]
fn sweep_expired_orders_filtered_owner_filter_stack_merge_and_exact_boundary() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_700_001_i64;
    let now = ctx.now_unix();
    let one_day = 86_400_u64;

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.ceo_id = char_id;
            __corp.name = "Test Market Corp".into();
            __corp.ticker = "TMKC".into();
            __corp.set_division_balance(1, IskCents::from_isk(1_000_000));
            __corp };
        corp.set_division_balance(3, IskCents::from_isk(200_000));
        s.corporations.insert(corp_id, corp);

        s.items.retain(|_, i| !(i.owner_id == char_id && i.type_id == 34));
        s.items.insert(770_001, InventoryItemRecord {
            item_id: 770_001, type_id: 34, owner_id: char_id, location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar, quantity: 500, singleton: false, version: 1, ..Default::default()
        });

        for (oid, owner, is_bid, price, vol_in, vol_rem, esc, issue_ts, ak) in [
            (880_001, char_id, false, 5, 300, 250, 0, now - one_day, None),
            (880_002, char_id, false, 5, 100, 100, 0, now - one_day + 1, None),
            (880_003, other_char, false, 5, 100, 100, 0, now - 2 * one_day, None),
            (880_004, corp_id, true, 10, 1_000, 1_000, 10_000, now - 2 * one_day, Some(1002)),
        ] {
            s.market_orders.insert(oid, MarketOrderRecord {
                order_id: oid, owner_id: owner, station_id: JITA_STATION_ID, region_id: THE_FORGE_REGION_ID,
                type_id: 34, is_bid, price: IskCents::from_isk(price), vol_entered: vol_in,
                vol_remaining: vol_rem, min_volume: 1, is_npc: false, escrow: IskCents::from_isk(esc),
            });
            save_market_order_meta(s, oid, MarketOrderMeta {
                range: 32_767, duration: 1, issue_unix: issue_ts, order_state: None,
                account_key: ak, final_vol_remaining: None, closed_unix: None,
            });
        }
    });

    let swept = ctx.db.with_mut_snapshot(|s| {
        sweep_expired_orders_filtered(s, now, Some((char_id, Some(corp_id))))
    });
    assert_eq!(swept.len(), 2);

    ctx.db.with_snapshot(|s| {
        let stack = &s.items[&770_001];
        assert_eq!((stack.quantity, stack.version), (750, 2));
        assert_eq!(s.market_orders[&880_002].vol_remaining, 100);
        assert_eq!(s.market_orders[&880_003].vol_remaining, 100);
        assert_eq!(s.corporations[&corp_id].division_balance(3), IskCents::from_isk(210_000));
    });

    sweep_and_notify_expired_for_char(&ctx, other_char);
    ctx.db.with_snapshot(|s| assert_eq!(s.market_orders[&880_003].vol_remaining, 0));
}

#[test]
fn market_proxy_corp_orders_history_and_remote_modify_validation() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = 98_700_002_i64;
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        s.characters.get_mut(&char_id).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(50_000_000);
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.ceo_id = char_id;
            __corp.name = "Modify Corp".into();
            __corp.ticker = "MODC".into();
            __corp.set_division_balance(1, IskCents::from_isk(10_000_000));
            __corp };
        corp.set_division_balance(2, IskCents::from_isk(5_000_000));
        corp.member_roles.insert(char_id, corp_roles::ROLE_TRADER);
        s.corporations.insert(corp_id, corp);

        for (oid, price_isk) in [(890_001, 20), (890_002, 12), (890_003, 18)] {
            s.market_orders.insert(oid, MarketOrderRecord {
                order_id: oid, owner_id: char_id, station_id: JITA_STATION_ID, region_id: THE_FORGE_REGION_ID,
                type_id: 34, is_bid: false, price: IskCents::from_isk(price_isk), vol_entered: 100,
                vol_remaining: 100, min_volume: 1, is_npc: false, escrow: IskCents(0),
            });
            save_market_order_meta(s, oid, MarketOrderMeta {
                range: 32_767, duration: 90, issue_unix: now.saturating_sub(600),
                order_state: None, account_key: None, final_vol_remaining: None, closed_unix: None,
            });
        }

        s.market_orders.insert(890_010, MarketOrderRecord {
            order_id: 890_010, owner_id: corp_id, station_id: JITA_STATION_ID, region_id: THE_FORGE_REGION_ID,
            type_id: 35, is_bid: true, price: IskCents::from_isk(100), vol_entered: 100,
            vol_remaining: 100, min_volume: 1, is_npc: false, escrow: IskCents::from_isk(5_000),
        });
        save_market_order_meta(s, 890_010, MarketOrderMeta {
            range: 5, duration: 90, issue_unix: now.saturating_sub(600),
            order_state: None, account_key: Some(1001), final_vol_remaining: None, closed_unix: None,
        });

        for (oid, closed_ago) in [
            (890_020, 100 * 86_400), (890_021, 10 * 86_400),
            (890_022, 5 * 86_400), (890_023, 2 * 86_400),
        ] {
            s.market_orders.insert(oid, MarketOrderRecord {
                order_id: oid, owner_id: char_id, station_id: JITA_STATION_ID, region_id: THE_FORGE_REGION_ID,
                type_id: 34, is_bid: true, price: IskCents::from_isk(10), vol_entered: 50,
                vol_remaining: 0, min_volume: 1, is_npc: false, escrow: IskCents(0),
            });
            save_market_order_meta(s, oid, MarketOrderMeta {
                range: -1, duration: 30, issue_unix: now.saturating_sub(closed_ago + 86_400),
                order_state: None, account_key: None, final_vol_remaining: Some(10),
                closed_unix: Some(now.saturating_sub(closed_ago)),
            });
        }
        for (eid, etype, reason) in [
            (901, "MarketOrderCancelRefund", "Refund for order 890021"),
            (902, "MarketOrderExpireRefund", "Refund for order 890022"),
        ] {
            s.wallet_journal.push(WalletJournalEntry {
                entry_id: eid, character_id: char_id, entry_type: etype.into(),
                amount: IskCents::from_isk(100), balance_after: IskCents::from_isk(1_000), reason: reason.into(),
            });
        }
    });

    assert!(handle(&ctx, &mut sess, "marketProxy", "GetSystemAsks", &[], &kw).unwrap().unwrap().is_cindexed_rowset());
    assert_eq!(handle(&ctx, &mut sess, "marketProxy", "GetCorporationOrders", &[], &kw).unwrap().unwrap().row_count(), Some(1));
    assert_eq!(handle(&ctx, &mut sess, "marketProxy", "GetMarketOrderHistory", &[], &kw).unwrap().unwrap().row_count(), Some(3));

    let mod_res = handle(&ctx, &mut sess, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(890_010), PyRep::Real(80.0)], &kw).unwrap().unwrap();
    assert_eq!(mod_res.dict_get("newPrice").and_then(PyRep::as_f64), Some(80.0));
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&890_010].price, IskCents::from_isk(80));
        assert_eq!(s.market_orders[&890_010].escrow, IskCents::from_isk(4_000));
    });

    ctx.db.with_mut_snapshot(|s| {
        let mut m = load_market_order_meta(s, &s.market_orders[&890_010], now);
        m.issue_unix = now.saturating_sub(600);
        save_market_order_meta(s, 890_010, m);
    });
    sess.station_id = Some(60_000_001);
    sess.solar_system_id = Some(30_000_144);

    assert_eq!(
        handle(&ctx, &mut sess, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(890_010), PyRep::Real(85.0)], &kw)
            .unwrap().unwrap_err().code,
        "DaytradingSkillRequired"
    );

    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(char_id).or_default().insert(16595, TrainedSkillRecord { type_id: 16595, level: 2, skill_points: 1_415.0 });
    });
    let kw_range = HashMap::from([("newRange".to_string(), PyRep::Integer(5))]);
    assert_eq!(
        handle(&ctx, &mut sess, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(890_010), PyRep::Real(85.0)], &kw_range)
            .unwrap().unwrap_err().code,
        "VisibilitySkillRequired"
    );

    ctx.db.with_mut_snapshot(|s| {
        s.skills.entry(char_id).or_default().insert(3447, TrainedSkillRecord { type_id: 3447, level: 3, skill_points: 8_000.0 });
    });
    let rem_ok = handle(&ctx, &mut sess, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(890_010), PyRep::Real(85.0)], &kw_range).unwrap().unwrap();
    assert_eq!(rem_ok.dict_get("range").and_then(PyRep::as_i64), Some(5));
}

#[test]
fn plex_sell_order_and_contract_proxy_escrow_and_lookup_branches() {
    let ctx = test_ctx();
    let seller_id = 90_000_001_i64;
    let rich_buyer = 90_000_002_i64;
    let broke_buyer = 90_000_003_i64;
    let buyer_corp = 98_700_010_i64;
    let mut sess = jita_session(seller_id, buyer_corp);
    let kw = HashMap::new();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.retain(|_, o| o.type_id != PLEX_TYPE_ID);
        let cur_plex = s.plex_vault_balance(seller_id);
        let _ = s.adjust_plex_vault(seller_id, -cur_plex);
        s.characters.get_mut(&seller_id).unwrap().balance = IskCents::from_isk(100_000_000);

        let base_char = s.characters[&seller_id].clone();
        for (cid, uid, bal) in [(rich_buyer, 102, 50_000_000), (broke_buyer, 103, 0)] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.balance = IskCents::from_isk(bal);
            s.characters.insert(cid, ch);
        }

        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = buyer_corp;
            __corp.ceo_id = rich_buyer;
            __corp.name = "PLEX Buyer Corp".into();
            __corp.ticker = "PLBC".into();
            __corp.set_division_balance(1, IskCents::from_isk(30_000_000));
            __corp };
        corp.set_division_balance(2, IskCents::from_isk(30_000_000));
        s.corporations.insert(buyer_corp, corp);

        for (iid, tid, owner, loc, flag, qty, sing) in [
            (795_001, PLEX_TYPE_ID, seller_id, JITA_STATION_ID, InventoryFlag::Hangar, 10, false),
            (910_001, 3293, seller_id, JITA_STATION_ID, InventoryFlag::Locked, 1, true),
            (910_002, 34, seller_id, 910_001, InventoryFlag::Cargo, 42, false),
            (910_003, 34, buyer_corp, JITA_STATION_ID, InventoryFlag::Locked, 500, false),
        ] {
            s.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: owner, location_id: loc,
                flag, quantity: qty, singleton: sing, version: 1, ..Default::default()
            });
        }

        for (oid, owner, price, esc, ak) in [
            (895_001, broke_buyer, 5_200_000, 1_000_000, None),
            (895_002, buyer_corp, 5_100_000, 2_200_000, Some(1001)),
            (895_003, rich_buyer, 5_000_000, 2_000_000, None),
        ] {
            s.market_orders.insert(oid, MarketOrderRecord {
                order_id: oid, owner_id: owner, station_id: JITA_STATION_ID, region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID, is_bid: true, price: IskCents::from_isk(price), vol_entered: 2,
                vol_remaining: 2, min_volume: 1, is_npc: false, escrow: IskCents::from_isk(esc),
            });
            if ak.is_some() {
                save_market_order_meta(s, oid, MarketOrderMeta {
                    range: 32_767, duration: 90, issue_unix: now - 60, order_state: None,
                    account_key: ak, final_vol_remaining: None, closed_unix: None,
                });
            }
        }

        s.contracts.clear();
        s.contracts.insert(920_001, ContractRecord {
            contract_id: 920_001, issuer_id: seller_id, issuer_corp_id: buyer_corp,
            start_station_id: JITA_STATION_ID, end_station_id: JITA_STATION_ID,
            contract_type: "Auction".into(), status: "Outstanding".into(), for_corp: true,
            expiration_unix: now.saturating_sub(3_600), price: IskCents::from_isk(1_000_000),
            item_ids: vec![910_003], ..Default::default()
        });
        s.contracts.insert(920_002, ContractRecord {
            contract_id: 920_002, issuer_id: seller_id, issuer_corp_id: buyer_corp,
            acceptor_id: Some(seller_id), start_station_id: JITA_STATION_ID, end_station_id: JITA_STATION_ID,
            contract_type: "Courier".into(), status: "InProgress".into(), expiration_unix: now + 86_400,
            reward: IskCents::from_isk(250_000), collateral: IskCents::from_isk(800_000),
            item_ids: vec![910_002], ..Default::default()
        });
    });

    let kv = HashMap::from([
        ("price".to_string(), PyRep::Real(5_000_000.0)),
        ("quantity".to_string(), PyRep::Integer(5)),
        ("duration".to_string(), PyRep::Integer(14)),
    ]);
    let res = handle(&ctx, &mut sess, "marketProxy", "PlacePlexSellOrder", &[PyRep::KeyVal(kv)], &kw).unwrap().unwrap();
    let rest_oid = res.dict_get("orderID").and_then(PyRep::as_i64).unwrap();

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&795_001].quantity, 5);
        assert_eq!(s.market_orders[&895_001].vol_remaining, 0);
        assert_eq!(s.characters[&broke_buyer].balance, IskCents::from_isk(1_000_000));
        assert_eq!(s.corporations[&buyer_corp].division_balance(2), IskCents::from_isk(22_000_000));
        assert_eq!(s.characters[&rich_buyer].balance, IskCents::from_isk(42_000_000));
        assert_eq!(s.market_orders[&rest_oid].vol_remaining, 1);
    });

    for m in ["GetPlexNewPriceHistory", "GetPlexOldPriceHistory"] {
        assert!(handle(&ctx, &mut sess, "marketProxy", m, &[], &kw).unwrap().is_ok());
    }

    assert_eq!(handle(&ctx, &mut sess, "contractProxy", "FinishAuction", &[PyRep::Integer(920_001)], &kw).unwrap().unwrap(), PyRep::Bool(true));
    for lookup_id in [920_002_i64, 910_002, 910_001] {
        let found = handle(&ctx, &mut sess, "contractProxy", "GetCourierContractFromItemID", &[PyRep::Integer(lookup_id)], &kw).unwrap().unwrap();
        assert_eq!(found.dict_get("contractID").and_then(PyRep::as_i64), Some(920_002));
    }
    let escrow = handle(&ctx, &mut sess, "contractProxy", "GetMyContractEscrow", &[], &kw).unwrap().unwrap();
    assert_eq!(escrow.dict_get("iskEscrow").and_then(PyRep::as_f64), Some(1_050_000.0));
}

#[test]
fn sweep_expired_order_notifications_and_handle_edge_mutants() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_700_020_i64;
    let mut sess = jita_session(char_id, corp_id);
    let kw = HashMap::new();
    let now = ctx.now_unix();

    ctx.db.provision_gm_account("mkt_edge_p", "pw", char_id, "MktEdge");
    let uid = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    let _ = ctx.db.acquire_online_lock(uid, sess.session_id, Some(char_id), true, now);

    // 1. sweep_expired_orders_and_notify returns 0 when nothing expired
    ctx.db.with_mut_snapshot(|s| s.market_orders.clear());
    assert_eq!(sweep_expired_orders_and_notify(&ctx, now), 0);

    // 2. Seed rich mix of expired orders to verify exact return count and every OnAccountChange / OnItemChange / OnMarketItemReceived branch
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().corporation_id = corp_id;
        s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(100_000);
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.ceo_id = char_id;
        corp.name = "Notify Corp".into();
        corp.ticker = "NCRP".into();
        corp.set_division_balance(1, IskCents::from_isk(500_000));
        corp.set_division_balance(3, IskCents::from_isk(700_000));
        corp.member_roles.insert(char_id, corp_roles::ROLE_TRADER);
        s.corporations.insert(corp_id, corp);

        // Clear demo items for char_id/corp_id so returned stacks match exact fixture expectations
        s.items.retain(|_, it| it.owner_id != char_id && it.owner_id != corp_id);
        // Decoys that must NOT match returned item stack lookup in emit_expired_order_notifications
        s.items.insert(930_001, InventoryItemRecord {
            item_id: 930_001, type_id: 34, owner_id: char_id, location_id: JITA_STATION_ID,
            flag: InventoryFlag::Cargo, quantity: 999, singleton: false, version: 1, ..Default::default()
        });
        s.items.insert(930_002, InventoryItemRecord {
            item_id: 930_002, type_id: 34, owner_id: char_id, location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar, quantity: 998, singleton: true, version: 1, ..Default::default()
        });
        // Existing stack of 20 Tritanium for char_id in Hangar -> after +5 return becomes 25 (> ret_qty branch -> col 9 = 20)
        s.items.insert(930_003, InventoryItemRecord {
            item_id: 930_003, type_id: 34, owner_id: char_id, location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar, quantity: 20, singleton: false, version: 1, ..Default::default()
        });

        let expired_specs: &[(i64, i64, bool, u32, i64, i32, i64, Option<u16>)] = &[
            // (oid, owner, is_bid, tid, price_cents, vol_rem, esc_cents, acct_key)
            // Personal bid with price > 0 & vol_rem > 0 -> OnAccountChange cash
            (940_001, char_id, true, 34, 10_00, 10, 100_00, None),
            // Corp bid with account_key=1002 (div 3) -> OnAccountChange cash3 for corp_id
            (940_002, corp_id, true, 34, 20_00, 5, 100_00, Some(1002)),
            // Zero-price bid -> NO OnAccountChange (`o.price.0 > 0`)
            (940_003, char_id, true, 35, 0, 5, 0, None),
            // Personal sell order on type 34 (merges into 930_003: 20 + 5 = 25 > 5 -> change col 9 = 20)
            (940_004, char_id, false, 34, 5_00, 5, 0, None),
            // Personal sell order on type 36 (NO existing stack -> creates 8 -> rec.quantity == ret_qty -> change col 3 = 0)
            (940_005, char_id, false, 36, 5_00, 8, 0, None),
            // Corp sell order on type 37 -> target_flag = Deliveries -> change col 3 = 0
            (940_006, corp_id, false, 37, 5_00, 4, 0, Some(1000)),
            // Personal PLEX sell order -> skips OnItemChange for `!is_corp && type_id == PLEX_TYPE_ID`, still sends OnMarketItemReceived
            (940_007, char_id, false, PLEX_TYPE_ID, 5_000_000_00, 2, 0, None),
        ];
        for &(oid, owner, is_bid, tid, price_cents, vol_rem, esc_cents, ak) in expired_specs {
            s.market_orders.insert(oid, MarketOrderRecord {
                order_id: oid, owner_id: owner, station_id: JITA_STATION_ID, region_id: THE_FORGE_REGION_ID,
                type_id: tid, is_bid, price: IskCents(price_cents), vol_entered: vol_rem,
                vol_remaining: vol_rem, min_volume: 1, is_npc: false, escrow: IskCents(esc_cents),
            });
            save_market_order_meta(s, oid, MarketOrderMeta {
                range: 32_767, duration: 1, issue_unix: now.saturating_sub(2 * 86_400),
                order_state: None, account_key: ak, final_vol_remaining: None, closed_unix: None,
            });
        }
        // Zero-price test bypasses other_char; add corp PLEX sell order (is_corp && PLEX) so `!is_corp` negation mutant fails
        s.market_orders.insert(940_008, MarketOrderRecord {
            order_id: 940_008, owner_id: corp_id, station_id: JITA_STATION_ID, region_id: THE_FORGE_REGION_ID,
            type_id: PLEX_TYPE_ID, is_bid: false, price: IskCents::from_isk(5_000_000), vol_entered: 3,
            vol_remaining: 3, min_volume: 1, is_npc: false, escrow: IskCents(0),
        });
        save_market_order_meta(s, 940_008, MarketOrderMeta {
            range: 32_767, duration: 1, issue_unix: now.saturating_sub(2 * 86_400),
            order_state: None, account_key: Some(1000), final_vol_remaining: None, closed_unix: None,
        });
    });

    let _ = ctx.drain_notifications_for_char(char_id);
    let swept_cnt = sweep_expired_orders_and_notify(&ctx, now);
    assert_eq!(swept_cnt, 8);
    assert_eq!(sweep_expired_orders_and_notify(&ctx, now), 0);

    let notifs = ctx.drain_notifications_for_char(char_id);
    // Personal bid (cash) + Corp bid div 3 (cash3) only — NOT 3 accounts (zero-price skipped)
    let acct_keys: Vec<(i64, String)> = notifs
        .iter()
        .filter(|n| n.event == "OnAccountChange")
        .filter_map(|n| Some((n.args.get(1)?.as_i64()?, n.args.first()?.as_str()?.to_string())))
        .collect();
    assert!(acct_keys.contains(&(char_id, "cash".to_string())), "got {acct_keys:?}");
    assert!(acct_keys.contains(&(corp_id, "cash3".to_string())), "got {acct_keys:?}");
    assert_eq!(acct_keys.len(), 2, "zero-price bid must not emit OnAccountChange: {acct_keys:?}");

    // OnMarketItemReceived emitted for all 5 sell orders (940_004..=940_008)
    let mkt_recv_oids: Vec<i64> = notifs
        .iter()
        .filter(|n| n.event == "OnMarketItemReceived")
        .filter_map(|n| n.args.first()?.as_i64())
        .collect();
    for expected_oid in [940_004_i64, 940_005, 940_006, 940_007, 940_008] {
        assert!(mkt_recv_oids.contains(&expected_oid), "missing {expected_oid}");
    }

    // OnItemChange emitted for Tritanium (type 34, merge diff col 9 = 20), Mexallon (type 36, new diff col 3 = 0),
    // Isogen corp deliveries (type 37, col 3 = 0), Corp PLEX (type 44992), and NOT personal PLEX
    let item_changes: Vec<(u32, i64, Vec<(i64, i64)>)> = notifs
        .iter()
        .filter(|n| n.event == "OnItemChange")
        .filter_map(|n| {
            let (tid, owner) = match &n.args[0] {
                PyRep::DBRow { columns, .. } => {
                    let tid = columns.iter().find(|(k, _)| k == "typeID")?.1.as_i64()? as u32;
                    let own = columns.iter().find(|(k, _)| k == "ownerID")?.1.as_i64()?;
                    (tid, own)
                }
                _ => return None,
            };
            let cols = match &n.args[1] {
                PyRep::Dict(entries) => entries
                    .iter()
                    .filter_map(|(k, v)| Some((k.as_i64()?, v.as_i64()?)))
                    .collect(),
                _ => return None,
            };
            Some((tid, owner, cols))
        })
        .collect();
    assert!(item_changes.contains(&(34, char_id, vec![(9, 20)])), "{item_changes:?}");
    assert!(item_changes.contains(&(36, char_id, vec![(3, 0)])), "{item_changes:?}");
    assert!(item_changes.contains(&(37, corp_id, vec![(3, 0)])), "{item_changes:?}");
    assert!(item_changes.contains(&(PLEX_TYPE_ID, corp_id, vec![(3, 0)])), "{item_changes:?}");
    assert!(!item_changes.iter().any(|(tid, own, _)| *tid == PLEX_TYPE_ID && *own == char_id));

    // Also test emit_expired_order_notifications directly with zero-remaining sell order & final_vol_remaining == 0 bid
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.insert(940_020, MarketOrderRecord {
            order_id: 940_020, owner_id: char_id, station_id: JITA_STATION_ID, region_id: THE_FORGE_REGION_ID,
            type_id: 34, is_bid: true, price: IskCents::from_isk(100), vol_entered: 10, vol_remaining: 0,
            min_volume: 1, is_npc: false, escrow: IskCents(0),
        });
        save_market_order_meta(s, 940_020, MarketOrderMeta {
            range: 32_767, duration: 1, issue_unix: now - 100, order_state: Some(4),
            account_key: None, final_vol_remaining: Some(0), closed_unix: Some(now),
        });
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    emit_expired_order_notifications(&ctx, &[(940_020, char_id, true, 34, JITA_STATION_ID)]);
    let dir_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(!dir_notifs.iter().any(|n| n.event == "OnAccountChange"));
    assert!(dir_notifs.iter().any(|n| n.event == "OnOwnOrdersChanged"));

    // 3. `handle` unknown service -> None; PlacePlexSellOrder KeyVal price Integer / non-finite / bad quantity guards;
    //    ModifyPlexCharOrder non-PLEX rejection & success; GetItemsInContainer; GetCourierContractFromItemID None.
    assert!(handle(&ctx, &mut sess, "nonexistent_svc", "Ping", &[], &kw).is_none());

    let kv_nan = PyRep::KeyVal(HashMap::from([
        ("price".into(), PyRep::Real(f64::NAN)),
        ("quantity".into(), PyRep::Integer(1)),
    ]));
    assert_eq!(
        handle(&ctx, &mut sess, "marketProxy", "PlacePlexSellOrder", &[kv_nan], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidMarketOrder"
    );
    let kv_bad_qty = PyRep::KeyVal(HashMap::from([
        ("price".into(), PyRep::Integer(5_000_000)),
        ("quantity".into(), PyRep::String("bad".into())),
    ]));
    assert_eq!(
        handle(&ctx, &mut sess, "marketProxy", "PlacePlexSellOrder", &[kv_bad_qty], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidMarketOrder"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(1_000_000);
        s.items.insert(930_100, InventoryItemRecord {
            item_id: 930_100, type_id: PLEX_TYPE_ID, owner_id: char_id, location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar, quantity: 5, singleton: false, version: 1, ..Default::default()
        });
    });
    let kv_int_ok = PyRep::KeyVal(HashMap::from([
        ("price".into(), PyRep::Integer(6_000_000)),
        ("quantity".into(), PyRep::Integer(2)),
    ]));
    let plex_order_rep = handle(&ctx, &mut sess, "marketProxy", "PlacePlexSellOrder", &[kv_int_ok], &kw)
        .unwrap()
        .unwrap();
    let plex_oid = plex_order_rep.dict_get("orderID").and_then(PyRep::as_i64).unwrap();
    // ModifyPlexCharOrder succeeds on PLEX order once past cooldown, rejects non-PLEX order
    ctx.db.with_mut_snapshot(|s| {
        let mut m = load_market_order_meta(s, &s.market_orders[&plex_oid], now);
        m.issue_unix = now.saturating_sub(600);
        save_market_order_meta(s, plex_oid, m);
        s.market_orders.insert(940_050, MarketOrderRecord {
            order_id: 940_050, owner_id: char_id, station_id: JITA_STATION_ID, region_id: THE_FORGE_REGION_ID,
            type_id: 34, is_bid: false, price: IskCents::from_isk(10), vol_entered: 5, vol_remaining: 5,
            min_volume: 1, is_npc: false, escrow: IskCents(0),
        });
        save_market_order_meta(s, 940_050, MarketOrderMeta {
            range: 32_767, duration: 90, issue_unix: now.saturating_sub(600),
            order_state: None, account_key: None, final_vol_remaining: None, closed_unix: None,
        });
    });
    assert_eq!(
        handle(&ctx, &mut sess, "marketProxy", "ModifyPlexCharOrder", &[PyRep::Integer(940_050), PyRep::Real(12.0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidPlexOrder"
    );
    assert!(
        handle(&ctx, &mut sess, "marketProxy", "ModifyPlexCharOrder", &[PyRep::Integer(plex_oid), PyRep::Real(5_900_000.0)], &kw)
            .unwrap()
            .is_ok()
    );

    // GetItemsInContainer returns owned/corp items in container_id (arg 0 and arg 1) and ignores foreign items
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(930_201, InventoryItemRecord {
            item_id: 930_201, type_id: 34, owner_id: char_id, location_id: 955_000,
            flag: InventoryFlag::Cargo, quantity: 42, singleton: false, ..Default::default()
        });
        s.items.insert(930_202, InventoryItemRecord {
            item_id: 930_202, type_id: 35, owner_id: other_char, location_id: 955_000,
            flag: InventoryFlag::Cargo, quantity: 99, singleton: false, ..Default::default()
        });
    });
    let in_cont0 = handle(&ctx, &mut sess, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(955_000)], &kw)
        .unwrap()
        .unwrap();
    match &in_cont0 {
        PyRep::List(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].dict_get("itemID").and_then(PyRep::as_i64), Some(930_201));
        }
        other => panic!("expected List, got {other:?}"),
    }
    let in_cont1 = handle(
        &ctx,
        &mut sess,
        "contractProxy",
        "GetItemsInContainer",
        &[PyRep::Integer(char_id), PyRep::Integer(955_000)],
        &kw,
    )
    .unwrap()
    .unwrap();
    match &in_cont1 {
        PyRep::List(rows) => assert_eq!(rows.len(), 1),
        other => panic!("expected List, got {other:?}"),
    }

    // Unknown courier item -> PyRep::None
    assert_eq!(
        handle(&ctx, &mut sess, "contractProxy", "GetCourierContractFromItemID", &[PyRep::Integer(888_777)], &kw)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
}
