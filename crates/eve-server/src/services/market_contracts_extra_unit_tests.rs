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
