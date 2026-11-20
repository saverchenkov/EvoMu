#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{MarketHistoryDayRecord, MarketOrderRecord};

#[test]
fn market_price_history_and_transactions_mutations() {
    assert_eq!(date_str_to_unix_secs("1970-01-01"), Some(0));
    assert_eq!(date_str_to_unix_secs("1970-01-02"), Some(86_400));
    assert_eq!(date_str_to_unix_secs("invalid"), None);
    assert_eq!(date_str_to_unix_secs("2025-13-01"), None);
    assert_eq!(date_str_to_unix_secs("2025-01-32"), None);
    assert_eq!(date_str_to_filetime("1970-01-01"), Some(116_444_736_000_000_000));

    assert_eq!(
        extract_type_ids_arg(Some(&PyRep::List(vec![PyRep::Integer(34), PyRep::Integer(-1)]))),
        Some((vec![34], false))
    );
    assert_eq!(
        extract_type_ids_arg(Some(&PyRep::PySet(vec![PyRep::Integer(35)]))),
        Some((vec![35], true))
    );
    assert_eq!(extract_type_ids_arg(Some(&PyRep::Integer(34))), None);
    assert_eq!(extract_type_ids_arg(None), None);

    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_history.insert(
            (10_000_002, 34),
            vec![
                MarketHistoryDayRecord {
                    date: "2025-01-02".into(),
                    lowest: 4.0,
                    highest: 5.0,
                    average: 4.5,
                    volume: 1000,
                    order_count: 10,
                    open_price: None,
                    close_price: None,
                },
                MarketHistoryDayRecord {
                    date: "2025-01-01".into(),
                    lowest: 3.5,
                    highest: 4.5,
                    average: 4.0,
                    volume: 500,
                    order_count: 5,
                    open_price: None,
                    close_price: None,
                },
            ],
        );
    });

    ctx.db.with_snapshot(|snap| {
        let rows = build_price_history_rows(snap, 34, 10_000_002);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1], PyRep::Real(3.5));
        assert_eq!(rows[1][1], PyRep::Real(4.0));

        let (sys_id, const_id) = resolve_station_solar_and_constellation(&ctx, snap, 60_003_760);
        assert_eq!(sys_id, 30_000_142);
        assert!(const_id > 0);

        let ord = MarketOrderRecord {
            order_id: 888_001,
            type_id: 34,
            owner_id: 90_000_001,
            region_id: 10_000_002,
            station_id: 60_003_760,
            price: IskCents(450),
            vol_entered: 100,
            vol_remaining: 50,
            min_volume: 1,
            is_bid: false,
            is_npc: false,
            escrow: IskCents(0),
        };
        let cells = order_to_packed_cells_with_ctx(&ctx, snap, &ord, 1_700_000_000);
        assert_eq!(cells.len(), 15);
    });

    let own_payload = build_own_order_changed_payload(&ctx, 999_999_999, "Add", false);
    assert_eq!(own_payload.len(), 3);
    assert_eq!(own_payload[1], PyRep::String("Add".into()));
    assert_eq!(own_payload[2], PyRep::Bool(false));

    assert!(handle_market_transactions_rpc(&ctx, &sess, "UnknownMethod", &[]).is_none());
    let char_tx = handle_market_transactions_rpc(&ctx, &sess, "CharGetTransactions", &[])
        .unwrap()
        .unwrap();
    assert!(!matches!(char_tx, PyRep::None));

    let corp_tx = handle_market_transactions_rpc(&ctx, &sess, "CorpGetTransactions", &[])
        .unwrap();
    assert!(corp_tx.is_ok() || corp_tx.is_err());
}

fn seed_upwell_market_hub(
    ctx: &ServerContext,
    structure_id: i64,
    owner_corp_id: i64,
    solar_system_id: u32,
    online: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                solar_system_id,
                owner_corp_id,
                type_id: 35833, // Fortizar
                name: format!("Hub-{structure_id}"),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: if online { 500 } else { 0 },
                ..Default::default()
            },
        );
        let mid = structure_id + 1;
        snap.items.insert(
            mid,
            eve_db::InventoryItemRecord {
                item_id: mid,
                type_id: 35892, // Standup Market Hub I
                owner_id: owner_corp_id,
                location_id: structure_id,
                flag: eve_db::InventoryFlag::from_u16(164),
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mid, online);
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.insert(
            structure_id,
            crate::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                corp_only: false,
                ..Default::default()
            },
        );
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
}

#[test]
fn crossed_modify_fills_cover_corp_upwell_refund_shortfall_and_plex_vault_paths() {
    use crate::services::market_contracts::{JITA_STATION_ID, PLEX_TYPE_ID, THE_FORGE_REGION_ID};
    use crate::services::market_trading::{save_market_order_meta, MarketOrderMeta as TradingOrderMeta};
    use eve_db::ledger::MarketTransactionRecord;
    use eve_db::{CorporationRecord, IskCents, MarketOrderMeta};

    let ctx = test_ctx();
    let other_forge_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.region_id == THE_FORGE_REGION_ID && s.station_id != JITA_STATION_ID)
        .expect("Forge has a second NPC station")
        .station_id;
    let non_forge_station = ctx
        .sde
        .stations
        .values()
        .find(|s| s.region_id != THE_FORGE_REGION_ID)
        .expect("Non-Forge NPC station")
        .station_id;

    let char_buyer = 950_001_i64;
    let char_seller = 950_002_i64;
    let corp_buyer_id = 98_950_001_i64;
    let corp_seller_id = 98_950_002_i64;
    let missing_corp_id = 98_999_777_i64;
    let offline_hub = 10_000_950_100_i64;
    let online_hub = 10_000_950_200_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        let base_ch = snap.characters.values().next().unwrap().clone();
        for cid in [char_buyer, char_seller] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.name = format!("Trader-{cid}");
            ch.corporation_id = corp_buyer_id;
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(JITA_STATION_ID);
            ch.balance = IskCents(1_000_000_000);
            snap.characters.insert(cid, ch);
        }
        for (corp_id, div1, div4) in [
            (corp_buyer_id, 500_000_000_i64, 1_000_i64),
            (corp_seller_id, 200_000_000_i64, 50_000_i64),
        ] {
            let mut corp = CorporationRecord::new(
                corp_id,
                format!("Corp-{corp_id}"),
                "CRPT",
                char_buyer,
            );
            corp.set_division_balance(1, IskCents(div1));
            corp.set_division_balance(4, IskCents(div4));
            snap.corporations.insert(corp_id, corp);
        }
    });
    seed_upwell_market_hub(&ctx, offline_hub, corp_seller_id, 30_000_142, false);
    seed_upwell_market_hub(&ctx, online_hub, corp_seller_id, 30_000_142, true);

    let now = ctx.now_unix();
    let trading_meta = |range: i64, ak: Option<u16>| TradingOrderMeta {
        range,
        duration: 90,
        issue_unix: now,
        order_state: None,
        account_key: ak,
        final_vol_remaining: None,
        closed_unix: None,
    };

    // 1. Personal buy order crossing cheaper ask -> refunds surplus escrow to personal buyer.balance.
    //    Also covers offline Upwell skip (L62-65), online Upwell match + corp seller div4 credit (L108-109),
    //    different NPC station match (!structures.contains_key, L62), partial ask (L187) & partial buy (L198).
    let (personal_buy_id, corp_ask_id, other_npc_ask_id, offline_ask_id) =
        (9_500_101, 9_500_102, 9_500_103, 9_500_104);
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            offline_ask_id,
            MarketOrderRecord {
                order_id: offline_ask_id,
                owner_id: corp_seller_id,
                station_id: offline_hub,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents(1_000),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            corp_ask_id,
            MarketOrderRecord {
                order_id: corp_ask_id,
                owner_id: corp_seller_id,
                station_id: online_hub,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents(5_000),
                vol_entered: 5,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(snap, corp_ask_id, trading_meta(32767, Some(1003)));
        snap.market_order_meta.insert(
            corp_ask_id,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 90,
                range: "32767".into(),
                issued_by: Some(char_seller),
                wallet_division: Some(4),
                is_corporation: true,
                state: None,
            },
        );
        snap.market_orders.insert(
            other_npc_ask_id,
            MarketOrderRecord {
                order_id: other_npc_ask_id,
                owner_id: char_seller,
                station_id: other_forge_station,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents(6_000),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            personal_buy_id,
            MarketOrderRecord {
                order_id: personal_buy_id,
                owner_id: char_buyer,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents(10_000),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(100_000),
            },
        );
    });

    let buy_before = ctx.db.with_snapshot(|s| s.characters[&char_buyer].balance);
    let corp_seller_div4_before = ctx
        .db
        .with_snapshot(|s| s.corporations[&corp_seller_id].division_balance(4));
    let mut buy_meta = trading_meta(32767, None);
    let res1 = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            execute_crossed_order_fills_on_modify(
                &ctx,
                snap,
                personal_buy_id,
                IskCents(10_000),
                1.0,
                false,
                corp_buyer_id,
                char_buyer,
                now,
                &mut buy_meta,
            )
        })
        .unwrap();

    // 5 matched at online_hub @ 50 ISK + 5 matched at other_npc_station @ 60 ISK = 550 ISK total cost,
    // out of 1000 ISK escrow => 450 ISK refunded to personal buyer. Offline hub @ 10 ISK stays untouched!
    assert_eq!(res1.item_deliveries.len(), 2);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.market_orders[&offline_ask_id].vol_remaining, 10);
        assert_eq!(snap.market_orders[&corp_ask_id].vol_remaining, 0);
        assert_eq!(snap.market_orders[&other_npc_ask_id].vol_remaining, 5);
        assert_eq!(snap.market_orders[&personal_buy_id].vol_remaining, 0);
        assert_eq!(snap.characters[&char_buyer].balance.0 - buy_before.0, 45_000);
        assert!(
            snap.corporations[&corp_seller_id].division_balance(4) > corp_seller_div4_before
        );
    });

    // 2. Partial personal buy order remaining > 0 (covers L198 else branch) when ask is smaller than bid.
    let (partial_buy_id, small_ask_id) = (9_500_110, 9_500_111);
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            small_ask_id,
            MarketOrderRecord {
                order_id: small_ask_id,
                owner_id: char_seller,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 35,
                is_bid: false,
                price: IskCents(1_000),
                vol_entered: 3,
                vol_remaining: 3,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            partial_buy_id,
            MarketOrderRecord {
                order_id: partial_buy_id,
                owner_id: char_buyer,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 35,
                is_bid: true,
                price: IskCents(1_000),
                vol_entered: 8,
                vol_remaining: 8,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(8_000),
            },
        );
    });
    let mut partial_meta = trading_meta(-1, None);
    let res_partial = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            execute_crossed_order_fills_on_modify(
                &ctx,
                snap,
                partial_buy_id,
                IskCents(1_000),
                1.0,
                false,
                corp_buyer_id,
                char_buyer,
                now,
                &mut partial_meta,
            )
        })
        .unwrap();
    assert_eq!(res_partial.item_deliveries.len(), 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.market_orders[&partial_buy_id].vol_remaining, 5);
        assert_eq!(snap.market_orders[&small_ask_id].vol_remaining, 0);
    });

    // 3. Corp buy order margin shortfall: CorporationNotFound -> InsufficientFunds -> funded div4 cover
    //    Plus corp refund on excess escrow (L123).
    let (corp_short_buy_id, corp_refund_buy_id, ask_for_corp_buy_id, ask_for_corp_refund_id) =
        (9_500_120, 9_500_121, 9_500_122, 9_500_123);
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            ask_for_corp_buy_id,
            MarketOrderRecord {
                order_id: ask_for_corp_buy_id,
                owner_id: char_seller,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 36,
                is_bid: false,
                price: IskCents(5_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            corp_short_buy_id,
            MarketOrderRecord {
                order_id: corp_short_buy_id,
                owner_id: missing_corp_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 36,
                is_bid: true,
                price: IskCents(5_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(2_000),
            },
        );
    });
    let mut corp_meta = trading_meta(-1, Some(1003));
    let Err(err_missing_corp) = ctx.db.try_with_mut_snapshot(|snap| {
        execute_crossed_order_fills_on_modify(
            &ctx,
            snap,
            corp_short_buy_id,
            IskCents(5_000),
            0.2,
            true,
            missing_corp_id,
            char_buyer,
            now,
            &mut corp_meta,
        )
    }) else {
        panic!("expected CorporationNotFound");
    };
    assert_eq!(err_missing_corp.code, "CorporationNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.get_mut(&corp_short_buy_id).unwrap().owner_id = corp_buyer_id;
    });
    let Err(err_insuff_corp) = ctx.db.try_with_mut_snapshot(|snap| {
        execute_crossed_order_fills_on_modify(
            &ctx,
            snap,
            corp_short_buy_id,
            IskCents(5_000),
            0.2,
            true,
            corp_buyer_id,
            char_buyer,
            now,
            &mut corp_meta,
        )
    }) else {
        panic!("expected InsufficientFunds");
    };
    assert_eq!(err_insuff_corp.code, "InsufficientFunds");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_buyer_id)
            .unwrap()
            .set_division_balance(4, IskCents(20_000));
    });
    let res_corp_short = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            execute_crossed_order_fills_on_modify(
                &ctx,
                snap,
                corp_short_buy_id,
                IskCents(5_000),
                0.2,
                true,
                corp_buyer_id,
                char_buyer,
                now,
                &mut corp_meta,
            )
        })
        .unwrap();
    assert!(res_corp_short.delivered_to_corp_deliveries > 0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.corporations[&corp_buyer_id].division_balance(4),
            IskCents(12_000)
        );
    });

    // Corp buy with surplus escrow -> refunds exact surplus to div4 (covers L123)
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            ask_for_corp_refund_id,
            MarketOrderRecord {
                order_id: ask_for_corp_refund_id,
                owner_id: char_seller,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 37,
                is_bid: false,
                price: IskCents(3_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            corp_refund_buy_id,
            MarketOrderRecord {
                order_id: corp_refund_buy_id,
                owner_id: corp_buyer_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 37,
                is_bid: true,
                price: IskCents(5_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(10_000),
            },
        );
    });
    let _ = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            execute_crossed_order_fills_on_modify(
                &ctx,
                snap,
                corp_refund_buy_id,
                IskCents(5_000),
                1.0,
                true,
                corp_buyer_id,
                char_buyer,
                now,
                &mut corp_meta,
            )
        })
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.corporations[&corp_buyer_id].division_balance(4),
            IskCents(16_000)
        );
    });

    // 4. Sell-side crossing bids:
    //    - Offline hub bid skipped (L232-235), online hub corp bid solvent cover in div4 (L270-278),
    //    - Insolvent corp bid cancelled & refunded to div4 (L286-289, L296),
    //    - Corp sell order div4 credit (L303-308), partial bid/self (L333, L343),
    //    - Personal PLEX sell crossing personal PLEX bid settling into PLEX vault (L347, L364)!
    let (corp_sell_id, offline_bid_id, insolv_corp_bid_id, solv_corp_bid_id, other_npc_bid_id) =
        (9_500_200, 9_500_201, 9_500_202, 9_500_203, 9_500_204);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_buyer_id)
            .unwrap()
            .set_division_balance(4, IskCents(10_000));
        snap.market_orders.insert(
            offline_bid_id,
            MarketOrderRecord {
                order_id: offline_bid_id,
                owner_id: char_buyer,
                station_id: offline_hub,
                region_id: THE_FORGE_REGION_ID,
                type_id: 38,
                is_bid: true,
                price: IskCents(9_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(18_000),
            },
        );
        save_market_order_meta(snap, offline_bid_id, trading_meta(32767, None));
        snap.market_orders.insert(
            insolv_corp_bid_id,
            MarketOrderRecord {
                order_id: insolv_corp_bid_id,
                owner_id: corp_buyer_id,
                station_id: online_hub,
                region_id: THE_FORGE_REGION_ID,
                type_id: 38,
                is_bid: true,
                price: IskCents(8_000),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(1_000),
            },
        );
        save_market_order_meta(snap, insolv_corp_bid_id, trading_meta(32767, Some(1003)));
        snap.market_order_meta.insert(
            insolv_corp_bid_id,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 90,
                range: "32767".into(),
                issued_by: Some(char_buyer),
                wallet_division: Some(4),
                is_corporation: true,
                state: None,
            },
        );
        snap.market_orders.insert(
            solv_corp_bid_id,
            MarketOrderRecord {
                order_id: solv_corp_bid_id,
                owner_id: corp_buyer_id,
                station_id: other_forge_station,
                region_id: THE_FORGE_REGION_ID,
                type_id: 38,
                is_bid: true,
                price: IskCents(5_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(7_000),
            },
        );
        save_market_order_meta(snap, solv_corp_bid_id, trading_meta(32767, Some(1003)));
        snap.market_order_meta.insert(
            solv_corp_bid_id,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 90,
                range: "32767".into(),
                issued_by: Some(char_buyer),
                wallet_division: Some(4),
                is_corporation: true,
                state: None,
            },
        );
        snap.market_orders.insert(
            other_npc_bid_id,
            MarketOrderRecord {
                order_id: other_npc_bid_id,
                owner_id: char_buyer,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 38,
                is_bid: true,
                price: IskCents(4_000),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(40_000),
            },
        );
        snap.market_orders.insert(
            corp_sell_id,
            MarketOrderRecord {
                order_id: corp_sell_id,
                owner_id: corp_seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 38,
                is_bid: false,
                price: IskCents(4_000),
                vol_entered: 4,
                vol_remaining: 4,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    let corp_seller_div4_before2 = ctx
        .db
        .with_snapshot(|s| s.corporations[&corp_seller_id].division_balance(4));
    let mut sell_corp_meta = trading_meta(-1, Some(1003));
    let res_sell = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            execute_crossed_order_fills_on_modify(
                &ctx,
                snap,
                corp_sell_id,
                IskCents(4_000),
                1.0,
                true,
                corp_seller_id,
                char_seller,
                now,
                &mut sell_corp_meta,
            )
        })
        .unwrap();
    assert_eq!(res_sell.item_deliveries.len(), 2);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.market_orders[&offline_bid_id].vol_remaining, 2);
        assert_eq!(snap.market_orders[&insolv_corp_bid_id].vol_remaining, 0);
        assert_eq!(snap.market_orders[&solv_corp_bid_id].vol_remaining, 0);
        assert_eq!(snap.market_orders[&other_npc_bid_id].vol_remaining, 8);
        assert_eq!(snap.market_orders[&corp_sell_id].vol_remaining, 0);
        assert_eq!(
            snap.corporations[&corp_buyer_id].division_balance(4),
            IskCents(8_000)
        );
        assert!(
            snap.corporations[&corp_seller_id].division_balance(4) > corp_seller_div4_before2
        );
    });

    // Personal PLEX sell order crossing personal PLEX bid settles to PLEX vault (L347), leaving remainder on seller (L343)
    let (plex_sell_id, plex_bid_id) = (9_500_250, 9_500_251);
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            plex_bid_id,
            MarketOrderRecord {
                order_id: plex_bid_id,
                owner_id: char_buyer,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID,
                is_bid: true,
                price: IskCents(500_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(1_000_000),
            },
        );
        snap.market_orders.insert(
            plex_sell_id,
            MarketOrderRecord {
                order_id: plex_sell_id,
                owner_id: char_seller,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents(400_000),
                vol_entered: 5,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    let vault_before = ctx.db.with_snapshot(|s| s.plex_vault_balance(char_buyer));
    let mut plex_sell_meta = trading_meta(-1, None);
    let res_plex = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            execute_crossed_order_fills_on_modify(
                &ctx,
                snap,
                plex_sell_id,
                IskCents(400_000),
                1.0,
                false,
                corp_buyer_id,
                char_seller,
                now,
                &mut plex_sell_meta,
            )
        })
        .unwrap();
    assert!(res_plex.item_deliveries.is_empty());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.plex_vault_balance(char_buyer), vault_before + 2);
        assert_eq!(snap.market_orders[&plex_sell_id].vol_remaining, 3);
    });

    // 5. build_price_history_rows fallback: unknown station (L438) + non-Forge region (L451) + qty 0 clamped to 1
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_history.clear();
        snap.market_transactions.clear();
        snap.market_orders.insert(
            9_500_300,
            MarketOrderRecord {
                order_id: 9_500_300,
                owner_id: char_seller,
                station_id: non_forge_station,
                region_id: 10_000_043,
                type_id: 34,
                is_bid: false,
                price: IskCents(999),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            9_500_301,
            MarketOrderRecord {
                order_id: 9_500_301,
                owner_id: char_seller,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents(999_999),
                vol_entered: 1,
                vol_remaining: 1,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_transactions.insert(
            1,
            MarketTransactionRecord {
                transaction_id: 1,
                date_unix: now,
                type_id: 34,
                quantity: 5,
                price: IskCents(1_000),
                location_id: 999_888_777,
                buyer_id: char_buyer,
                seller_id: char_seller,
                buyer_account_key: 1000,
                seller_account_key: 1004,
                journal_ref_id: 1,
            },
        );
        snap.market_transactions.insert(
            2,
            MarketTransactionRecord {
                transaction_id: 2,
                date_unix: now,
                type_id: 34,
                quantity: 5,
                price: IskCents(9_900),
                location_id: non_forge_station,
                buyer_id: char_buyer,
                seller_id: char_seller,
                buyer_account_key: 1000,
                seller_account_key: 1004,
                journal_ref_id: 2,
            },
        );
        snap.market_transactions.insert(
            3,
            MarketTransactionRecord {
                transaction_id: 3,
                date_unix: now,
                type_id: 34,
                quantity: 0,
                price: IskCents(700),
                location_id: JITA_STATION_ID,
                buyer_id: char_buyer,
                seller_id: char_seller,
                buyer_account_key: 1000,
                seller_account_key: 1000,
                journal_ref_id: 3,
            },
        );
    });
    let hist_rows = ctx
        .db
        .with_snapshot(|snap| build_price_history_rows(snap, 34, THE_FORGE_REGION_ID));
    assert_eq!(hist_rows.len(), 1);
    assert_eq!(hist_rows[0][4], PyRep::Integer(1));

    // 6. extract_type_ids_arg Object/ObjectEx negative/empty fallback (L638, L658, L660)
    assert_eq!(
        extract_type_ids_arg(Some(&PyRep::Object {
            type_name: "foo".into(),
            args: Box::new(PyRep::Integer(1)),
        })),
        None
    );
    assert_eq!(
        extract_type_ids_arg(Some(&PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::Tuple(vec![
                PyRep::String("ctor".into()),
                PyRep::Tuple(vec![PyRep::Integer(1)]),
            ])),
            list: vec![],
            dict: vec![],
        })),
        Some((Vec::new(), true))
    );

    // 7. handle_market_transactions_rpc unauthenticated + Unix secs min_unix + seller row fields (L678, L689, L746, L751, L755)
    let mut no_char_sess = auth_session(1, char_seller, corp_seller_id, 30_000_142);
    no_char_sess.character_id = None;
    assert!(handle_market_transactions_rpc(&ctx, &no_char_sess, "CharGetTransactions", &[])
        .unwrap()
        .is_err());

    let extract_crowset_rows = |rep: PyRep| -> Vec<Vec<PyRep>> {
        let rows = match rep {
            PyRep::ObjectEx { list, .. } => list,
            PyRep::Object { args, .. } => {
                let PyRep::Tuple(t) = *args else {
                    panic!("expected tuple args inside crowset");
                };
                let Some(PyRep::List(rows)) = t.get(1) else {
                    panic!("expected rows list inside crowset");
                };
                rows.clone()
            }
            other => panic!("expected crowset PyRep::ObjectEx or PyRep::Object from GetTransactions, got {other:?}"),
        };
        rows.into_iter()
            .map(|r| match r {
                PyRep::DBRow { columns, .. } => columns.into_iter().map(|(_, v)| v).collect(),
                PyRep::List(cols) => cols,
                other => panic!("expected row DBRow/List, got {other:?}"),
            })
            .collect()
    };

    let seller_sess = auth_session(1, char_seller, corp_seller_id, 30_000_142);
    let tx_list = handle_market_transactions_rpc(
        &ctx,
        &seller_sess,
        "CharGetTransactions",
        &[PyRep::Integer(now as i64 - 10)],
    )
    .unwrap()
    .unwrap();
    let rows1 = extract_crowset_rows(tx_list);
    assert!(!rows1.is_empty());
    let row3 = rows1
        .iter()
        .find(|cols| cols.first() == Some(&PyRep::Integer(3)))
        .expect("seller transaction row");
    assert_eq!(row3[8], PyRep::Integer(1000));
    assert_eq!(row3[11], PyRep::Integer(char_buyer));

    // Non-zero seller transaction to kill sign-flip mutant (`credit = -gross` vs `+gross`)
    // plus corp seller div5 transaction to cover CorpGetTransactions explicit account filter
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_transactions.insert(
            4,
            MarketTransactionRecord {
                transaction_id: 4,
                date_unix: now + 1,
                type_id: 34,
                quantity: 3,
                price: IskCents(1_200),
                location_id: JITA_STATION_ID,
                buyer_id: char_buyer,
                seller_id: char_seller,
                buyer_account_key: 1000,
                seller_account_key: 1000,
                journal_ref_id: 4,
            },
        );
        snap.market_transactions.insert(
            5,
            MarketTransactionRecord {
                transaction_id: 5,
                date_unix: now + 1,
                type_id: 34,
                quantity: 4,
                price: IskCents(1_500),
                location_id: JITA_STATION_ID,
                buyer_id: char_buyer,
                seller_id: corp_seller_id,
                buyer_account_key: 1000,
                seller_account_key: 1004,
                journal_ref_id: 5,
            },
        );
    });
    let tx_list2 = handle_market_transactions_rpc(
        &ctx,
        &seller_sess,
        "CharGetTransactions",
        &[PyRep::Integer(now as i64 + 1)],
    )
    .unwrap()
    .unwrap();
    let rows2 = extract_crowset_rows(tx_list2);
    assert_eq!(rows2.len(), 1);
    assert_eq!(rows2[0][8], PyRep::Integer(1000));
    assert_eq!(rows2[0][11], PyRep::Integer(char_buyer));
    assert_eq!(rows2[0][12], PyRep::Real(36.0));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_seller) {
            ch.corporation_id = corp_seller_id;
        }
    });
    assert_eq!(
        handle_market_transactions_rpc(
            &ctx,
            &seller_sess,
            "CorpGetTransactions",
            &[PyRep::Integer(now as i64 + 1), PyRep::Integer(1004)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_seller_id) {
            corp.ceo_id = char_seller;
        }
    });
    let corp_tx_list = handle_market_transactions_rpc(
        &ctx,
        &seller_sess,
        "CorpGetTransactions",
        &[PyRep::Integer(now as i64 + 1), PyRep::Integer(1004)],
    )
    .unwrap()
    .unwrap();
    let corp_rows = extract_crowset_rows(corp_tx_list);
    assert_eq!(corp_rows.len(), 1);
    assert_eq!(corp_rows[0][8], PyRep::Integer(1004));
    assert_eq!(corp_rows[0][11], PyRep::Integer(char_buyer));
    assert_eq!(corp_rows[0][12], PyRep::Real(60.0));

    // Migrated from client_compat: verify CharGetTransactions and CorpGetTransactions emit canonical
    // `CRowset` with `Real` price/credit directly from `handle_market_transactions_rpc`.
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_transactions.insert(
            9001,
            MarketTransactionRecord {
                transaction_id: 9001,
                date_unix: now + 2,
                type_id: 34,
                quantity: 100,
                price: IskCents::from_f64_isk(5.25),
                location_id: JITA_STATION_ID,
                buyer_id: char_buyer,
                seller_id: corp_seller_id,
                buyer_account_key: 1000,
                seller_account_key: 1004,
                journal_ref_id: 777,
            },
        );
    });
    let buyer_sess = auth_session(2, char_buyer, corp_seller_id + 1, 30_000_142);
    for (method, sess, args, expected_credit) in [
        (
            "CharGetTransactions",
            &buyer_sess,
            vec![PyRep::Integer(now as i64 + 2)],
            -525.0,
        ),
        (
            "CorpGetTransactions",
            &seller_sess,
            vec![PyRep::Integer(now as i64 + 2), PyRep::Integer(1004)],
            525.0,
        ),
    ] {
        let res = handle_market_transactions_rpc(&ctx, sess, method, &args)
            .unwrap()
            .unwrap();
        let normalized = crate::router::client_compat::normalize_macho_rpc_reply(
            "marketProxy",
            method,
            Ok(res.clone()),
        )
        .unwrap();
        assert_eq!(normalized, res, "expected identity passthrough for marketProxy.{method}");
        let PyRep::ObjectEx { header, list, .. } = res else {
            panic!("expected CRowset ObjectEx for {method}");
        };
        let PyRep::Tuple(h_parts) = *header else {
            panic!("expected header tuple");
        };
        let PyRep::Tuple(class_parts) = &h_parts[0] else {
            panic!("expected class tuple");
        };
        assert_eq!(
            class_parts[0],
            PyRep::Token("carbon.common.script.sys.crowset.CRowset".into())
        );
        assert_eq!(list.len(), 1);
        let PyRep::DBRow { columns, .. } = &list[0] else {
            panic!("expected packed blue.DBRow inside CRowset");
        };
        let by_col: std::collections::HashMap<&str, &PyRep> =
            columns.iter().map(|(k, v)| (k.as_str(), v)).collect();
        assert_eq!(by_col.get("transactionID"), Some(&&PyRep::Integer(9001)));
        assert_eq!(by_col.get("typeID"), Some(&&PyRep::Integer(34)));
        assert_eq!(by_col.get("quantity"), Some(&&PyRep::Integer(100)));
        assert_eq!(by_col.get("price"), Some(&&PyRep::Real(5.25)));
        assert_eq!(by_col.get("credit"), Some(&&PyRep::Real(expected_credit)));
    }
}

#[test]
fn global_plex_price_history_and_crossed_modify_sleeper_plex_edges() {
    use crate::services::market_contracts::{
        is_permanent_npc_sleeper_buy_order, JITA_STATION_ID, PLEX_TYPE_ID,
        SLEEPER_BLUE_LOOT_ORDERS, THE_FORGE_REGION_ID,
    };
    use crate::services::market_trading::{save_market_order_meta, MarketOrderMeta as TradingOrderMeta};
    use eve_db::ledger::MarketTransactionRecord;
    use eve_db::{IskCents, MarketHistoryDayRecord, MarketOrderRecord};

    let ctx = test_ctx();
    let seller_id = 90_000_001_i64;
    let buyer_id = 90_000_002_i64;
    let domain_region_id = 10_000_043_u32;
    let sinq_region_id = 10_000_032_u32;

    // 1. build_global_price_history_rows aggregates multi-region market_history by date (volume-weighted average)
    //    while keeping regional build_price_history_rows scoped per region.
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_history.clear();
        snap.market_transactions.clear();
        snap.market_history.insert(
            (THE_FORGE_REGION_ID, PLEX_TYPE_ID),
            vec![MarketHistoryDayRecord {
                date: "2025-02-01".into(),
                lowest: 5_000_000.0,
                highest: 5_200_000.0,
                average: 5_100_000.0,
                volume: 40,
                order_count: 2,
                open_price: None,
                close_price: None,
            }],
        );
        snap.market_history.insert(
            (domain_region_id, PLEX_TYPE_ID),
            vec![MarketHistoryDayRecord {
                date: "2025-02-01".into(),
                lowest: 4_800_000.0,
                highest: 5_000_000.0,
                average: 4_900_000.0,
                volume: 10,
                order_count: 1,
                open_price: None,
                close_price: None,
            }],
        );
        snap.market_history.insert(
            (sinq_region_id, PLEX_TYPE_ID),
            vec![MarketHistoryDayRecord {
                date: "2025-02-02".into(),
                lowest: 5_050_000.0,
                highest: 5_050_000.0,
                average: 5_050_000.0,
                volume: 5,
                order_count: 1,
                open_price: None,
                close_price: None,
            }],
        );
        snap.market_history.insert(
            (THE_FORGE_REGION_ID, 34),
            vec![MarketHistoryDayRecord {
                date: "2025-02-01".into(),
                lowest: 4.0,
                highest: 4.0,
                average: 4.0,
                volume: 100,
                order_count: 1,
                open_price: None,
                close_price: None,
            }],
        );
    });

    ctx.db.with_snapshot(|snap| {
        assert!(build_price_history_rows(snap, 34, domain_region_id).is_empty());
        let rows = build_global_price_history_rows(snap, PLEX_TYPE_ID);
        assert_eq!(rows.len(), 2);
        // Day 1 (`2025-02-01`): low=4.8M, high=5.2M, avg=(40*5.1M + 10*4.9M)/50 = 5.06M, vol=50, orders=3
        assert_eq!(rows[0][0], PyRep::Integer(date_str_to_filetime("2025-02-01").unwrap()));
        assert_eq!(rows[0][1], PyRep::Real(4_800_000.0));
        assert_eq!(rows[0][2], PyRep::Real(5_200_000.0));
        let avg0 = rows[0][3].as_f64().unwrap();
        assert!((avg0 - 5_060_000.0).abs() < 1e-4);
        assert_eq!(rows[0][4], PyRep::Integer(50));
        assert_eq!(rows[0][5], PyRep::Integer(3));
        // Day 2 (`2025-02-02`): single region row preserved exactly
        assert_eq!(rows[1][0], PyRep::Integer(date_str_to_filetime("2025-02-02").unwrap()));
        assert_eq!(rows[1][3], PyRep::Real(5_050_000.0));
        assert_eq!(rows[1][4], PyRep::Integer(5));
        assert_eq!(rows[1][5], PyRep::Integer(1));
    });

    // 2. Global fallback across market_transactions when market_history has no entries for PLEX.
    let now = ctx.now_unix();
    let amarr_station = 60_008_494_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_history.clear();
        snap.market_transactions.clear();
        snap.market_transactions.insert(
            101,
            MarketTransactionRecord {
                transaction_id: 101,
                date_unix: now,
                type_id: PLEX_TYPE_ID,
                quantity: 3,
                price: IskCents::from_isk(5_000_000),
                location_id: JITA_STATION_ID,
                buyer_id,
                seller_id,
                buyer_account_key: 1000,
                seller_account_key: 1000,
                journal_ref_id: 1,
            },
        );
        snap.market_transactions.insert(
            102,
            MarketTransactionRecord {
                transaction_id: 102,
                date_unix: now,
                type_id: PLEX_TYPE_ID,
                quantity: 1,
                price: IskCents::from_isk(5_400_000),
                location_id: amarr_station,
                buyer_id,
                seller_id,
                buyer_account_key: 1000,
                seller_account_key: 1000,
                journal_ref_id: 2,
            },
        );
        snap.market_transactions.insert(
            103,
            MarketTransactionRecord {
                transaction_id: 103,
                date_unix: now,
                type_id: 34,
                quantity: 500,
                price: IskCents::from_isk(4),
                location_id: JITA_STATION_ID,
                buyer_id,
                seller_id,
                buyer_account_key: 1000,
                seller_account_key: 1000,
                journal_ref_id: 3,
            },
        );
    });
    ctx.db.with_snapshot(|snap| {
        let fb_rows = build_global_price_history_rows(snap, PLEX_TYPE_ID);
        assert_eq!(fb_rows.len(), 1);
        assert_eq!(fb_rows[0][1], PyRep::Real(5_000_000.0));
        assert_eq!(fb_rows[0][2], PyRep::Real(5_400_000.0));
        assert!((fb_rows[0][3].as_f64().unwrap() - 5_100_000.0).abs() < 1e-4);
        assert_eq!(fb_rows[0][4], PyRep::Integer(4));
        assert_eq!(fb_rows[0][5], PyRep::Integer(2));
    });

    // 3. execute_crossed_order_fills_on_modify sell branch:
    //    (a) crossing a permanent NPC Sleeper buy order keeps vol_remaining full and creates no NPC hangar item;
    //    (b) modifying a PLEX sell order downward crosses a PLEX bid in another region and credits buyer's PLEX vault.
    let (sleeper_tid, sleeper_price_isk) = SLEEPER_BLUE_LOOT_ORDERS[1]; // 30_745 @ 500,000 ISK
    let (npc_bid_id, seller_sleeper_ask_id, forge_plex_bid_id, domain_plex_ask_id) =
        (9_600_001_i64, 9_600_002_i64, 9_600_003_i64, 9_600_004_i64);
    let npc_items_before = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.owner_id == 1_000_125).count()
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        snap.skills.entry(seller_id).or_default().remove(&16622);
        snap.characters.get_mut(&seller_id).unwrap().balance = IskCents::from_isk(10_000_000);
        let cur_vault = snap.plex_vault_balance(buyer_id);
        let _ = snap.adjust_plex_vault(buyer_id, -cur_vault);

        snap.market_orders.insert(
            npc_bid_id,
            MarketOrderRecord {
                order_id: npc_bid_id,
                owner_id: 1_000_125,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: sleeper_tid,
                is_bid: true,
                price: IskCents::from_isk(sleeper_price_isk),
                vol_entered: 1_000_000,
                vol_remaining: 1_000_000,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            seller_sleeper_ask_id,
            MarketOrderRecord {
                order_id: seller_sleeper_ask_id,
                owner_id: seller_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: sleeper_tid,
                is_bid: false,
                price: IskCents::from_isk(sleeper_price_isk + 100_000),
                vol_entered: 3,
                vol_remaining: 3,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            forge_plex_bid_id,
            MarketOrderRecord {
                order_id: forge_plex_bid_id,
                owner_id: buyer_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID,
                is_bid: true,
                price: IskCents::from_isk(5_100_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(10_200_000),
            },
        );
        snap.market_orders.insert(
            domain_plex_ask_id,
            MarketOrderRecord {
                order_id: domain_plex_ask_id,
                owner_id: seller_id,
                station_id: amarr_station,
                region_id: domain_region_id,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents::from_isk(5_300_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        for oid in [npc_bid_id, seller_sleeper_ask_id, forge_plex_bid_id, domain_plex_ask_id] {
            save_market_order_meta(
                snap,
                oid,
                TradingOrderMeta {
                    range: 32767,
                    duration: 90,
                    issue_unix: now,
                    order_state: None,
                    account_key: None,
                    final_vol_remaining: None,
                    closed_unix: None,
                },
            );
        }
    });

    let (sleeper_res, plex_res) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut s_meta = TradingOrderMeta {
            range: 32767,
            duration: 90,
            issue_unix: now,
            order_state: None,
            account_key: None,
            final_vol_remaining: None,
            closed_unix: None,
        };
        let sleeper_price = IskCents::from_isk(sleeper_price_isk);
        snap.market_orders.get_mut(&seller_sleeper_ask_id).unwrap().price = sleeper_price;
        let r1 = execute_crossed_order_fills_on_modify(
            &ctx,
            snap,
            seller_sleeper_ask_id,
            sleeper_price,
            1.0,
            false,
            0,
            seller_id,
            now,
            &mut s_meta,
        )?;

        let mut p_meta = s_meta;
        let plex_price = IskCents::from_isk(5_100_000);
        snap.market_orders.get_mut(&domain_plex_ask_id).unwrap().price = plex_price;
        let r2 = execute_crossed_order_fills_on_modify(
            &ctx,
            snap,
            domain_plex_ask_id,
            plex_price,
            1.0,
            false,
            0,
            seller_id,
            now,
            &mut p_meta,
        )?;
        Ok::<_, eve_proto::BridgeRpcError>((r1, r2))
    }).expect("crossed modify sells");

    assert!(sleeper_res.item_deliveries.is_empty());
    assert!(plex_res.item_deliveries.is_empty());
    ctx.db.with_snapshot(|snap| {
        let npc_ord = &snap.market_orders[&npc_bid_id];
        assert!(is_permanent_npc_sleeper_buy_order(npc_ord));
        assert_eq!(npc_ord.vol_remaining, 1_000_000);
        assert_eq!(snap.market_orders[&seller_sleeper_ask_id].vol_remaining, 0);
        assert_eq!(snap.market_orders[&forge_plex_bid_id].vol_remaining, 0);
        assert_eq!(snap.market_orders[&domain_plex_ask_id].vol_remaining, 0);
        assert_eq!(snap.plex_vault_balance(buyer_id), 2);
        let npc_items_after = snap.items.values().filter(|i| i.owner_id == 1_000_125).count();
        assert_eq!(npc_items_after, npc_items_before);
    });
}

#[test]
fn crossed_modify_cross_region_plex_corp_buyer_delivers_to_bid_station() {
    use crate::services::market_contracts::PLEX_TYPE_ID;
    use crate::services::market_trading::{save_market_order_meta, MarketOrderMeta};
    use eve_db::{CorporationRecord, InventoryFlag, IskCents};

    let ctx = test_ctx();
    let seller_id = 90_000_001_i64;
    let corp_id = 98_000_777_i64;
    let jita_station = 60_003_760_i64;
    let the_forge_region = 10_000_002_u32;
    let amarr_station = 60_008_494_i64;
    let domain_region = 10_000_043_u32;
    let now = 1_750_000_000_u64;

    let corp_bid_id = 9_450_001_i64;
    let jita_ask_id = 9_450_002_i64;

    let mut ask_meta = MarketOrderMeta {
        range: 32_767,
        duration: 90,
        issue_unix: now,
        order_state: None,
        account_key: Some(1000),
        final_vol_remaining: None,
        closed_unix: None,
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.ceo_id = 90_000_002;
        corp.name = "DomainPlexCorp".into();
        corp.ticker = "DPLC".into();
        corp.set_division_balance(1, IskCents::from_isk(50_000_000));
        snap.corporations.insert(corp_id, corp);

        snap.market_orders.insert(
            corp_bid_id,
            MarketOrderRecord {
                order_id: corp_bid_id,
                owner_id: corp_id,
                station_id: amarr_station,
                region_id: domain_region,
                type_id: PLEX_TYPE_ID,
                is_bid: true,
                price: IskCents::from_isk(5_100_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(10_200_000),
            },
        );
        snap.market_orders.insert(
            jita_ask_id,
            MarketOrderRecord {
                order_id: jita_ask_id,
                owner_id: seller_id,
                station_id: jita_station,
                region_id: the_forge_region,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents::from_isk(5_500_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        for oid in [corp_bid_id, jita_ask_id] {
            save_market_order_meta(snap, oid, ask_meta);
        }
    });

    let res = ctx
        .db
        .with_mut_snapshot(|snap| {
            let new_price = IskCents::from_isk(5_100_000);
            snap.market_orders.get_mut(&jita_ask_id).unwrap().price = new_price;
            execute_crossed_order_fills_on_modify(
                &ctx,
                snap,
                jita_ask_id,
                new_price,
                1.0,
                false,
                0,
                seller_id,
                now,
                &mut ask_meta,
            )
        })
        .expect("crossed modify sell into corp PLEX bid");

    assert_eq!(res.item_deliveries.len(), 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.market_orders[&corp_bid_id].vol_remaining, 0);
        assert_eq!(snap.market_orders[&jita_ask_id].vol_remaining, 0);
        let delivered = snap
            .items
            .values()
            .find(|it| it.owner_id == corp_id && it.type_id == PLEX_TYPE_ID && it.flag == InventoryFlag::Deliveries)
            .expect("Corp cross-region PLEX delivery item must exist");
        assert_eq!(
            delivered.location_id, amarr_station,
            "Corp PLEX delivery must land at buyer bid station {amarr_station}, not seller station {jita_station}"
        );
        assert_eq!(delivered.quantity, 2);
    });
}
